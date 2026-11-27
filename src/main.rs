mod cli;
mod commands;
mod compress;
mod config;
mod dependency_layer;
mod direct_plan;
mod fetch;
mod hash;
mod lockfile;
mod lockfile_v13;
mod native;
mod native_store;
mod nixstore;
mod package_cache;
mod plan;
mod pnpm;
mod semver;
mod shared_store;
mod temp;
mod url;
mod verify;
mod workspace;

use anyhow::{Context, Result, ensure};
use commands::{Execution, Project};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(crate) enum Commands {
    Build {
        project: Project,
        out: Option<PathBuf>,
        no_link: bool,
    },
    Run {
        execution: Execution,
        script: String,
    },
    Test(Execution),
    Export {
        project: Project,
        out: Option<PathBuf>,
        include_sources: bool,
        plan_only: bool,
    },
    ProjectInputs {
        project: Project,
        sources: PathBuf,
        graph: PathBuf,
    },
    WorkspaceInfo {
        directory: PathBuf,
        list: bool,
        filters: Vec<String>,
    },
    RunScript {
        installation: PathBuf,
        project: Option<PathBuf>,
        importer: PathBuf,
        script: String,
        args: Vec<std::ffi::OsString>,
    },
    Fetch {
        directory: PathBuf,
        npmrc: Option<PathBuf>,
        all: bool,
        system: Option<String>,
        all_platforms: bool,
    },
    Verify {
        directory: PathBuf,
        store: Option<PathBuf>,
    },
    Materialize {
        directory: PathBuf,
        project: Option<PathBuf>,
        out: PathBuf,
    },
    Assemble {
        source: PathBuf,
        out: PathBuf,
        tarballs: PathBuf,
        keep_verification_store: bool,
        unpacked_store: Option<PathBuf>,
        package_contents: Option<PathBuf>,
        shared_packages: Option<PathBuf>,
    },
    PreparePackage {
        archive: PathBuf,
        out: PathBuf,
        id: String,
        integrity: String,
    },
    PatchPackage {
        source: PathBuf,
        out: PathBuf,
        patch: PathBuf,
        hash: String,
    },
    Attach {
        source: PathBuf,
        layer: PathBuf,
        out: PathBuf,
    },
    InternalPlan,
    InternalSymlink {
        target: PathBuf,
        out: PathBuf,
    },
    PrepareZstd {
        archive: PathBuf,
        out: PathBuf,
    },
}

#[derive(Default)]
pub(crate) struct BuildOptions {
    pub keep_verification_store: bool,
    pub zstd_archives: bool,
    pub cache_packages: bool,
    pub cache_dependencies: bool,
    pub shared_dependencies: bool,
}

impl BuildOptions {
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::json!({
            "keepVerificationStore": self.keep_verification_store,
            "zstdArchives": self.zstd_archives,
            "cachePackages": self.cache_packages,
            "cacheDependencies": self.cache_dependencies,
            "sharedDependencies": self.shared_dependencies,
        })
    }
}

struct Exported {
    primary: String,
    importers: usize,
    packages: usize,
    dependency_files: std::collections::BTreeSet<String>,
}

fn export(
    args: &Project,
    out: &Path,
    include_sources: bool,
    out_link: Option<&Path>,
    standalone: bool,
) -> Result<Exported> {
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
    let stage = temp::tempdir_in(parent)?;
    let projected = stage.path().join("project");
    let written = write_export(
        args,
        stage.path(),
        &projected,
        include_sources,
        out_link,
        standalone,
    )?;
    fs::rename(stage.path(), out)?;
    Ok(written)
}

fn project_inputs(args: &Project, sources: &Path, graph: &Path) -> Result<Exported> {
    fs::create_dir(graph)?;
    fs::create_dir(sources)?;
    let written = write_export(args, graph, sources, true, None, false)?;
    let metadata = graph.join("project");
    for name in &written.dependency_files {
        let target = metadata.join(name);
        fs::create_dir_all(target.parent().context("dependency input has no parent")?)?;
        fs::copy(sources.join(name), target)?;
    }
    Ok(written)
}

fn write_export(
    args: &Project,
    graph: &Path,
    projected: &Path,
    include_sources: bool,
    out_link: Option<&Path>,
    standalone: bool,
) -> Result<Exported> {
    let project = args.directory.canonicalize()?;
    let lock = workspace::read_lock(&project)?;
    let mut filters = args.selectors();
    let aliases = project.join("drvlang.json");
    if aliases.is_file() {
        let config: serde_json::Value = serde_json::from_slice(&fs::read(aliases)?)?;
        if let Some(commands) = config.get("commands").and_then(serde_json::Value::as_object) {
            for selector in &mut filters {
                if let Some(path) = commands.get(selector).and_then(serde_json::Value::as_str) {
                    *selector = path.to_owned();
                }
            }
        }
    }
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
    workspace::write_project(
        &project,
        projected,
        &selection,
        &lock_yaml,
        include_sources,
        out_link,
    )?;
    plan.target_packages = native::target_packages(projected, &selection.lock)?;
    plan.dependency_files = dependency_layer::files_for_lock(projected, &selection.lock)?;
    fs::write(
        graph.join("plan.json"),
        serde_json::to_vec_pretty(&plan.to_value())?,
    )?;
    fs::write(
        graph.join("options.json"),
        serde_json::to_vec_pretty(&args.options.to_value())?,
    )?;
    fs::write(graph.join("default.nix"), include_str!("../nix/build.nix"))?;
    if standalone {
        fs::create_dir_all(graph.join("drvjs-src/src"))?;
        fs::create_dir_all(graph.join("drvjs-src/nix"))?;
        for (name, content) in [
            ("Cargo.toml", include_str!("../Cargo.toml")),
            ("Cargo.lock", include_str!("../Cargo.lock")),
            ("src/cli.rs", include_str!("cli.rs")),
            ("src/main.rs", include_str!("main.rs")),
            ("src/commands.rs", include_str!("commands.rs")),
            ("src/compress.rs", include_str!("compress.rs")),
            ("src/config.rs", include_str!("config.rs")),
            (
                "src/dependency_layer.rs",
                include_str!("dependency_layer.rs"),
            ),
            ("src/direct_plan.rs", include_str!("direct_plan.rs")),
            ("src/fetch.rs", include_str!("fetch.rs")),
            ("src/hash.rs", include_str!("hash.rs")),
            ("src/lockfile.rs", include_str!("lockfile.rs")),
            ("src/lockfile_v13.rs", include_str!("lockfile_v13.rs")),
            ("src/nixstore.rs", include_str!("nixstore.rs")),
            ("src/plan.rs", include_str!("plan.rs")),
            ("src/pnpm.rs", include_str!("pnpm.rs")),
            ("src/semver.rs", include_str!("semver.rs")),
            ("src/shared_store.rs", include_str!("shared_store.rs")),
            ("src/temp.rs", include_str!("temp.rs")),
            ("src/native.rs", include_str!("native.rs")),
            ("src/native_store.rs", include_str!("native_store.rs")),
            ("src/package_cache.rs", include_str!("package_cache.rs")),
            ("src/native_shim.txt", include_str!("native_shim.txt")),
            ("src/url.rs", include_str!("url.rs")),
            ("src/verify.rs", include_str!("verify.rs")),
            ("src/workspace.rs", include_str!("workspace.rs")),
            ("nix/build.nix", include_str!("../nix/build.nix")),
            ("nix/package.nix", include_str!("../nix/package.nix")),
        ] {
            fs::write(graph.join("drvjs-src").join(name), content)?;
        }
    }
    Ok(Exported {
        primary: selection.primary,
        importers: selection.importers.len(),
        packages: plan.packages.len(),
        dependency_files: plan.dependency_files,
    })
}

fn main() {
    let command = match cli::parse() {
        Ok(cmd) => cmd,
        Err(error) => {
            eprintln!("error: {error:#}");
            std::process::exit(2);
        }
    };
    let result = match command {
        Commands::Build {
            project,
            out,
            no_link,
        } => commands::build(
            &project,
            if no_link {
                None
            } else {
                Some(out.as_deref().unwrap_or(Path::new("result")))
            },
        ),
        Commands::Run { execution, script } => execution.run(&script),
        Commands::Test(execution) => execution.run("test"),
        Commands::Export {
            project,
            out,
            include_sources,
            plan_only,
        } => {
            let out = out.unwrap_or_else(|| project.directory.join(".drvjs"));
            export(&project, &out, include_sources, None, !plan_only).map(|graph| {
                println!(
                    "Exported {} workspace importers and {} locked packages to {}",
                    graph.importers,
                    graph.packages,
                    out.display()
                );
            })
        }
        Commands::ProjectInputs {
            project,
            sources,
            graph,
        } => project_inputs(&project, &sources, &graph).map(|_| ()),
        Commands::WorkspaceInfo {
            directory,
            list,
            filters,
        } => commands::workspace_info(&directory, list, &filters),
        Commands::RunScript {
            installation,
            project,
            importer,
            script,
            args,
        } => commands::run_script(
            project.as_deref().unwrap_or(&installation),
            &installation,
            &importer,
            &script,
            &args,
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
        Commands::InternalPlan => direct_plan::internal_plan(),
        Commands::InternalSymlink { target, out } => direct_plan::internal_symlink(&target, &out),
        Commands::PrepareZstd { archive, out } => direct_plan::prepare_zstd(&archive, &out),
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
