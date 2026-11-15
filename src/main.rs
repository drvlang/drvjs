mod config;
mod dependency_layer;
mod fetch;
mod native;
mod native_store;
mod package_cache;
mod plan;
mod pnpm;
mod shared_store;
mod verify;
mod workspace;

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Parser)]
#[command(
    version,
    about = "Build pnpm node_modules with package-granular Nix fetches"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Export {
        #[arg(short = 'C', long, default_value = ".")]
        directory: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
        /// Exact workspace name or relative path; dependencies are included. Repeat to select several.
        #[arg(long = "filter")]
        filters: Vec<String>,
        /// Host-only credentials for URL dependencies that have no integrity hash.
        #[arg(long)]
        npmrc: Option<PathBuf>,
        /// Retain pnpm's package store for later offline `drvjs verify`.
        #[arg(long)]
        keep_verification_store: bool,
        /// Cache zstd-normalized archives in Nix (experimental).
        #[arg(long)]
        zstd_archives: bool,
        /// Cache extracted packages independently for repeated CI builds.
        #[arg(long)]
        cache_packages: bool,
        /// Cache the installed dependency tree across workspace source edits.
        #[arg(long)]
        cache_dependencies: bool,
        /// Share immutable registry packages between outputs (changes physical module paths).
        #[arg(long)]
        shared_dependencies: bool,
    },
    /// Import authenticated tarballs into Nix without storing credentials.
    Fetch {
        directory: PathBuf,
        #[arg(long)]
        npmrc: Option<PathBuf>,
        /// Fetch public tarballs too (default: custom/private registries only).
        #[arg(long)]
        all: bool,
        /// Target Nix system to prefetch (defaults to the current host).
        #[arg(long, conflicts_with = "all_platforms")]
        system: Option<String>,
        /// Prefetch every platform in the exported lockfile.
        #[arg(long)]
        all_platforms: bool,
    },
    /// Compare node_modules with pnpm's fresh offline reconstruction.
    Verify {
        directory: PathBuf,
        /// Use a compatible external pnpm v10 store; the original store is never modified.
        #[arg(long)]
        store: Option<PathBuf>,
    },
    /// Expand a shared installation into a standalone, writable directory.
    Materialize {
        directory: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
    },
    /// Internal Nix builder entry point.
    #[command(hide = true)]
    Assemble {
        source: PathBuf,
        out: PathBuf,
        tarballs: PathBuf,
        #[arg(long)]
        keep_verification_store: bool,
        /// Experimental: seed installation from a compatible retained pnpm store.
        #[arg(long)]
        unpacked_store: Option<PathBuf>,
        /// Internal manifest of immutable, Nix-built package contents.
        #[arg(long)]
        package_contents: Option<PathBuf>,
        /// Internal second Nix output for a shared registry graph.
        #[arg(long)]
        shared_packages: Option<PathBuf>,
    },
    #[command(hide = true)]
    PreparePackage {
        archive: PathBuf,
        out: PathBuf,
        id: String,
        integrity: String,
    },
    #[command(hide = true)]
    PatchPackage {
        source: PathBuf,
        out: PathBuf,
        patch: PathBuf,
        hash: String,
    },
    #[command(hide = true)]
    Attach {
        source: PathBuf,
        layer: PathBuf,
        out: PathBuf,
    },
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct BuildOptions {
    keep_verification_store: bool,
    zstd_archives: bool,
    cache_packages: bool,
    cache_dependencies: bool,
    shared_dependencies: bool,
}

fn export(
    project: &Path,
    out: &Path,
    filters: &[String],
    npmrc: Option<&Path>,
    options: BuildOptions,
) -> Result<()> {
    let project = project.canonicalize()?;
    let lock = fs::read_to_string(project.join("pnpm-lock.yaml"))?;
    let mut selection = workspace::select(&project, &lock, filters)?;
    fetch::complete_integrities(&mut selection.lock, npmrc)?;
    let settings = config::npmrc(&project)?;
    let mut plan = plan::parse_with_registry(&selection.lock, &settings)?;
    ensure!(
        !out.exists(),
        "export destination already exists: {}",
        out.display()
    );
    // Stage atomically so an unsuccessful export does not leave a usable-looking graph.
    let parent = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let stage = tempfile::tempdir_in(parent)?;
    workspace::write_project(&project, &stage.path().join("project"), &selection)?;
    plan.target_packages = native::target_packages(&stage.path().join("project"))?;
    plan.dependency_files = dependency_layer::files(&stage.path().join("project"))?;
    fs::write(
        stage.path().join("plan.json"),
        serde_json::to_vec_pretty(&plan)?,
    )?;
    fs::write(
        stage.path().join("options.json"),
        serde_json::to_vec_pretty(&options)?,
    )?;
    fs::write(
        stage.path().join("default.nix"),
        include_str!("../nix/build.nix"),
    )?;
    fs::create_dir_all(stage.path().join("drvjs-src/src"))?;
    fs::create_dir_all(stage.path().join("drvjs-src/nix"))?;
    for (name, content) in [
        ("Cargo.toml", include_str!("../Cargo.toml")),
        ("Cargo.lock", include_str!("../Cargo.lock")),
        ("src/main.rs", include_str!("main.rs")),
        ("src/config.rs", include_str!("config.rs")),
        (
            "src/dependency_layer.rs",
            include_str!("dependency_layer.rs"),
        ),
        ("src/fetch.rs", include_str!("fetch.rs")),
        ("src/plan.rs", include_str!("plan.rs")),
        ("src/pnpm.rs", include_str!("pnpm.rs")),
        ("src/shared_store.rs", include_str!("shared_store.rs")),
        ("src/native.rs", include_str!("native.rs")),
        ("src/native_store.rs", include_str!("native_store.rs")),
        ("src/package_cache.rs", include_str!("package_cache.rs")),
        ("src/native_shim.txt", include_str!("native_shim.txt")),
        ("src/verify.rs", include_str!("verify.rs")),
        ("src/workspace.rs", include_str!("workspace.rs")),
        ("nix/build.nix", include_str!("../nix/build.nix")),
        ("nix/package.nix", include_str!("../nix/package.nix")),
    ] {
        fs::write(stage.path().join("drvjs-src").join(name), content)?;
    }
    if let Some(mut hint) = std::env::var("DRVJS_NIX_HELPER")
        .ok()
        .and_then(|value| serde_json::from_str::<serde_json::Value>(&value).ok())
        && let Some(path) = hint["path"].as_str()
        && Path::new(path).parent() == Some(Path::new("/nix/store"))
        && std::env::current_exe()? == Path::new(path).join("bin/drvjs")
        && let Ok(output) = std::process::Command::new("nix-store")
            .args(["--query", "--deriver", path])
            .output()
        && output.status.success()
    {
        let deriver = String::from_utf8(output.stdout)?;
        let deriver = deriver.trim();
        if deriver.ends_with(".drv")
            && Path::new(deriver).parent() == Some(Path::new("/nix/store"))
        {
            hint["deriver"] = deriver.into();
            fs::write(stage.path().join("helper.json"), serde_json::to_vec(&hint)?)?;
        }
    }
    fs::rename(stage.path(), out)?;
    println!(
        "Exported {} workspace importers and {} locked packages to {}",
        selection.importers.len(),
        plan.packages.len(),
        out.display()
    );
    Ok(())
}

fn main() {
    let result = match Cli::parse().command {
        Commands::Export {
            directory,
            out,
            filters,
            npmrc,
            keep_verification_store,
            zstd_archives,
            cache_packages,
            cache_dependencies,
            shared_dependencies,
        } => export(
            &directory,
            &out,
            &filters,
            npmrc.as_deref(),
            BuildOptions {
                keep_verification_store,
                zstd_archives,
                cache_packages,
                cache_dependencies,
                shared_dependencies,
            },
        ),
        Commands::Fetch {
            directory,
            npmrc,
            all,
            system,
            all_platforms,
        } => fetch::fetch(
            &directory,
            npmrc.as_deref(),
            all,
            system.as_deref(),
            all_platforms,
        ),
        Commands::Verify { directory, store } => verify::verify(&directory, store.as_deref()),
        Commands::Materialize { directory, out } => dependency_layer::materialize(&directory, &out),
        Commands::Assemble {
            source,
            out,
            tarballs,
            keep_verification_store,
            unpacked_store,
            package_contents,
            shared_packages,
        } => native::assemble(
            &source,
            &out,
            &tarballs,
            keep_verification_store,
            unpacked_store.as_deref(),
            package_contents.as_deref(),
        )
        .and_then(|()| {
            if let Some(packages) = shared_packages {
                shared_store::split(&out, &packages)
            } else {
                Ok(())
            }
        }),
        Commands::PreparePackage {
            archive,
            out,
            id,
            integrity,
        } => package_cache::prepare(&archive, &out, &id, &integrity),
        Commands::PatchPackage {
            source,
            out,
            patch,
            hash,
        } => package_cache::patch(&source, &out, &patch, &hash),
        Commands::Attach { source, layer, out } => dependency_layer::attach(&source, &layer, &out),
    };
    if let Err(error) = result.context("drvjs failed") {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}
