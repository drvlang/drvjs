mod commands;
mod config;
mod dependency_layer;
mod fetch;
mod lockfile;
mod native;
mod native_store;
mod package_cache;
mod plan;
mod pnpm;
mod shared_store;
mod verify;
mod workspace;

use anyhow::{Context, Result, ensure};
use clap::{Args, Parser, Subcommand};
use commands::{Execution, Project};
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
    /// Build a project with its locked node_modules tree.
    Build {
        #[command(flatten)]
        project: Project,
        /// Nix output symlink (relative to the project).
        #[arg(short, long, default_value = "result")]
        out: PathBuf,
    },
    /// Run the start script in a writable copy of the built project.
    Run {
        #[command(flatten)]
        execution: Execution,
        /// Package script to run.
        #[arg(long, default_value = "start")]
        script: String,
    },
    /// Run the test script in a writable copy of the built project.
    Test(Execution),
    /// Write a standalone Nix build (defaults to .drvjs in the project).
    Export {
        #[command(flatten)]
        project: Project,
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Include application sources for a single-package project.
        #[arg(long)]
        include_sources: bool,
    },
    #[command(hide = true)]
    WorkspaceInfo {
        directory: PathBuf,
        #[arg(long)]
        list: bool,
        filters: Vec<String>,
    },
    #[command(hide = true)]
    RunScript {
        installation: PathBuf,
        importer: PathBuf,
        script: String,
        #[arg(last = true)]
        args: Vec<std::ffi::OsString>,
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
    /// Copy an installation or cached dependency layer into a standalone, writable workspace.
    Materialize {
        directory: PathBuf,
        /// Use the current export's project/ directory with a matching dependencies layer.
        #[arg(long)]
        project: Option<PathBuf>,
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

#[derive(Args, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct BuildOptions {
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
}

struct Exported {
    primary: String,
    importers: usize,
    packages: usize,
}

fn export(
    args: &Project,
    out: &Path,
    include_sources: bool,
    out_link: Option<&Path>,
) -> Result<Exported> {
    let project = args.directory.canonicalize()?;
    let lock = workspace::read_lock(&project)?;
    let filters = args.selectors();
    let mut selection = workspace::select(&project, &lock, &filters)?;
    let registry_urls = config::project_registries(
        &mut selection.lock,
        selection.config.as_mut(),
        &mut selection.npmrc,
    )?;
    let updated = fetch::complete_integrities(&mut selection.lock, args.npmrc.as_deref())?;
    // An unfiltered, single-package export preserves even comments and formatting.
    let lock_yaml = if !updated && !registry_urls && selection.preserve_lock_bytes {
        lock
    } else {
        selection.lock.sort_all_objects();
        serde_yaml_ng::to_string(&selection.lock)?
    };
    let mut plan = plan::from_lock(&selection.lock, &selection.npmrc)?;
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
    let projected = stage.path().join("project");
    workspace::write_project(
        &project,
        &projected,
        &selection,
        &lock_yaml,
        include_sources,
        out_link,
    )?;
    plan.target_packages = native::target_packages(&projected, &selection.lock)?;
    plan.dependency_files = dependency_layer::files_for_lock(&projected, &selection.lock)?;
    fs::write(
        stage.path().join("plan.json"),
        serde_json::to_vec_pretty(&plan)?,
    )?;
    fs::write(
        stage.path().join("options.json"),
        serde_json::to_vec_pretty(&args.options)?,
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
        ("src/commands.rs", include_str!("commands.rs")),
        ("src/config.rs", include_str!("config.rs")),
        (
            "src/dependency_layer.rs",
            include_str!("dependency_layer.rs"),
        ),
        ("src/fetch.rs", include_str!("fetch.rs")),
        ("src/lockfile.rs", include_str!("lockfile.rs")),
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
    fs::rename(stage.path(), out)?;
    Ok(Exported {
        primary: selection.primary,
        importers: selection.importers.len(),
        packages: plan.packages.len(),
    })
}

fn main() {
    let result = match Cli::parse().command {
        Commands::Build { project, out } => commands::build(&project, &out),
        Commands::Run { execution, script } => execution.run(&script),
        Commands::Test(execution) => execution.run("test"),
        Commands::Export {
            project,
            out,
            include_sources,
        } => {
            let out = out.unwrap_or_else(|| project.directory.join(".drvjs"));
            export(&project, &out, include_sources, None).map(|graph| {
                println!(
                    "Exported {} workspace importers and {} locked packages to {}",
                    graph.importers,
                    graph.packages,
                    out.display()
                );
            })
        }
        Commands::WorkspaceInfo {
            directory,
            list,
            filters,
        } => commands::workspace_info(&directory, list, &filters),
        Commands::RunScript {
            installation,
            importer,
            script,
            args,
        } => commands::run_script(&installation, &installation, &importer, &script, &args),
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
        Commands::Materialize {
            directory,
            project,
            out,
        } => dependency_layer::materialize(
            project.as_deref().unwrap_or(&directory),
            &directory,
            &out,
        ),
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
        std::process::exit(
            error
                .downcast_ref::<commands::ChildExit>()
                .map_or(1, |e| e.0),
        );
    }
}
