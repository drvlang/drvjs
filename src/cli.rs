//! Zero-dependency CLI argument parser replacing `clap` and `clap_derive`.
use crate::{
    BuildOptions, Commands,
    commands::{Execution, Project},
};
use anyhow::{Result, bail, ensure};
use std::{ffi::OsString, path::PathBuf};

const VERSION: &str = env!("CARGO_PKG_VERSION");

const TOP_HELP: &str = "\
Build pnpm node_modules with package-granular Nix fetches

Usage: drvjs <COMMAND>

Commands:
  build        Build a project with its locked node_modules tree.
  run          Run the start script in a writable copy of the built project.
  test         Run the test script in a writable copy of the built project.
  export       Write a standalone Nix build (defaults to .drvjs in the project).
  fetch        Import authenticated tarballs into Nix without storing credentials.
  verify       Compare node_modules with pnpm's fresh offline reconstruction.
  materialize  Copy an installation or cached dependency layer into a standalone, writable workspace.
  help         Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
";

fn subcommand_help(cmd: &str) -> Option<&'static str> {
    Some(match cmd {
        "build" => "\
Build a project with its locked node_modules tree.

Usage: drvjs build [OPTIONS] [PACKAGE]

Options:
  -C, --directory <DIRECTORY>  Project or workspace root containing pnpm-lock.yaml [default: .]
      --filter <FILTERS>       Include another workspace package and its dependencies
      --npmrc <NPMRC>          Host-only npm credentials for registry tarballs
      --keep-verification-store
      --zstd-archives
      --cache-packages
      --cache-dependencies
      --shared-dependencies
  -o, --out, --out-link <OUT>  Nix output symlink (relative to the project)
      --no-link                Print the store path without creating an output symlink
  -h, --help                   Print help
",
        "run" => "\
Run the start script in a writable copy of the built project.

Usage: drvjs run [OPTIONS] [PACKAGE] [-- <ARGS>...]

Options:
  -C, --directory <DIRECTORY>  Project or workspace root containing pnpm-lock.yaml [default: .]
      --filter <FILTERS>       Include another workspace package and its dependencies
      --npmrc <NPMRC>          Host-only npm credentials for registry tarballs
      --script <SCRIPT>        Package script to run [default: start]
      --keep-verification-store
      --zstd-archives
      --cache-packages
      --cache-dependencies
      --shared-dependencies
  -h, --help                   Print help
",
        "test" => "\
Run the test script in a writable copy of the built project.

Usage: drvjs test [OPTIONS] [PACKAGE] [-- <ARGS>...]

Options:
  -C, --directory <DIRECTORY>  Project or workspace root containing pnpm-lock.yaml [default: .]
      --filter <FILTERS>       Include another workspace package and its dependencies
      --npmrc <NPMRC>          Host-only npm credentials for registry tarballs
      --keep-verification-store
      --zstd-archives
      --cache-packages
      --cache-dependencies
      --shared-dependencies
  -h, --help                   Print help
",
        "export" => "\
Write a standalone Nix build (defaults to .drvjs in the project).

Usage: drvjs export [OPTIONS] [PACKAGE]

Options:
  -C, --directory <DIRECTORY>  Project or workspace root containing pnpm-lock.yaml [default: .]
      --filter <FILTERS>       Include another workspace package and its dependencies
      --npmrc <NPMRC>          Host-only npm credentials for registry tarballs
  -o, --out <OUT>              Output directory
      --include-sources        Include application sources for a single-package project
      --keep-verification-store
      --zstd-archives
      --cache-packages
      --cache-dependencies
      --shared-dependencies
  -h, --help                   Print help
",
        "fetch" => "\
Import authenticated tarballs into Nix without storing credentials.

Usage: drvjs fetch [OPTIONS] <DIRECTORY>

Options:
      --npmrc <NPMRC>
      --all
      --system <SYSTEM>
      --all-platforms
  -h, --help
",
        "verify" => "\
Compare node_modules with pnpm's fresh offline reconstruction.

Usage: drvjs verify [OPTIONS] <DIRECTORY>

Options:
      --store <STORE>
  -h, --help
",
        "materialize" => "\
Copy an installation or cached dependency layer into a standalone, writable workspace.

Usage: drvjs materialize [OPTIONS] --out <OUT> <DIRECTORY>

Options:
      --project <PROJECT>
  -o, --out <OUT>
  -h, --help
",
        _ => return None,
    })
}

fn next_val(
    args: &[OsString],
    i: &mut usize,
    inline: Option<&str>,
    flag: &str,
) -> Result<OsString> {
    if let Some(v) = inline {
        return Ok(OsString::from(v));
    }
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("missing value for {flag}"))
}

fn next_str(args: &[OsString], i: &mut usize, inline: Option<&str>, flag: &str) -> Result<String> {
    next_val(args, i, inline, flag)?
        .into_string()
        .map_err(|_| anyhow::anyhow!("non-UTF8 argument for {flag}"))
}

fn split_flag(arg: &str) -> (&str, Option<&str>) {
    if arg.starts_with("--") {
        match arg.split_once('=') {
            Some((k, v)) => (k, Some(v)),
            None => (arg, None),
        }
    } else if arg.starts_with('-') && arg.len() > 2 {
        (&arg[..2], Some(&arg[2..]))
    } else {
        (arg, None)
    }
}

fn try_project_flag(
    flag: &str,
    inline: Option<&str>,
    args: &[OsString],
    i: &mut usize,
    project: &mut Project,
) -> Result<bool> {
    match flag {
        "-C" | "--directory" => {
            project.directory = PathBuf::from(next_val(args, i, inline, flag)?);
            Ok(true)
        }
        "--filter" => {
            project.filters.push(next_str(args, i, inline, flag)?);
            Ok(true)
        }
        "--npmrc" => {
            project.npmrc = Some(PathBuf::from(next_val(args, i, inline, flag)?));
            Ok(true)
        }
        "--keep-verification-store" => {
            project.options.keep_verification_store = true;
            Ok(true)
        }
        "--zstd-archives" => {
            project.options.zstd_archives = true;
            Ok(true)
        }
        "--cache-packages" => {
            project.options.cache_packages = true;
            Ok(true)
        }
        "--cache-dependencies" => {
            project.options.cache_dependencies = true;
            Ok(true)
        }
        "--shared-dependencies" => {
            project.options.shared_dependencies = true;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn default_project() -> Project {
    Project {
        directory: PathBuf::from("."),
        package: None,
        filters: Vec::new(),
        npmrc: None,
        options: BuildOptions::default(),
    }
}

pub fn parse() -> Result<Commands> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.is_empty() {
        eprint!("{TOP_HELP}");
        std::process::exit(2);
    }
    let first = args[0].to_string_lossy();
    match first.as_ref() {
        "-h" | "--help" => {
            print!("{TOP_HELP}");
            std::process::exit(0);
        }
        "-V" | "--version" => {
            println!("drvjs {VERSION}");
            std::process::exit(0);
        }
        "help" => {
            if let Some(sub) = args.get(1) {
                let s = sub.to_string_lossy();
                if let Some(help) = subcommand_help(&s) {
                    print!("{help}");
                    std::process::exit(0);
                }
                bail!("unknown subcommand: {s}");
            }
            print!("{TOP_HELP}");
            std::process::exit(0);
        }
        _ => {}
    }

    let rest = &args[1..];
    // Handle -h/--help before "--" for known subcommands
    for a in rest {
        if a == "--" {
            break;
        }
        if a == "-h" || a == "--help" {
            if let Some(help) = subcommand_help(&first) {
                print!("{help}");
                std::process::exit(0);
            }
        }
    }

    match first.as_ref() {
        "build" => {
            let mut project = default_project();
            let mut out = None;
            let mut no_link = false;
            let mut i = 0;
            while i < rest.len() {
                let s = rest[i].to_string_lossy();
                if s.starts_with('-') && s != "-" {
                    let (flag, inline) = split_flag(&s);
                    if !try_project_flag(flag, inline, rest, &mut i, &mut project)? {
                        match flag {
                            "-o" | "--out" | "--out-link" => {
                                out = Some(PathBuf::from(next_val(rest, &mut i, inline, flag)?));
                            }
                            "--no-link" => no_link = true,
                            _ => bail!("unexpected argument {flag:?} for build"),
                        }
                    }
                } else {
                    ensure!(project.package.is_none(), "unexpected positional argument: {s}");
                    project.package = Some(rest[i].clone().into_string().map_err(|_| anyhow::anyhow!("non-UTF8 package"))?);
                }
                i += 1;
            }
            ensure!(!(no_link && out.is_some()), "--out cannot be used with --no-link");
            Ok(Commands::Build {
                project,
                out,
                no_link,
            })
        }
        "run" | "test" => {
            let is_run = first == "run";
            let mut project = default_project();
            let mut script = "start".to_owned();
            let mut trailing = Vec::new();
            let mut i = 0;
            while i < rest.len() {
                if rest[i] == "--" {
                    trailing.extend_from_slice(&rest[i + 1..]);
                    break;
                }
                let s = rest[i].to_string_lossy();
                if s.starts_with('-') && s != "-" {
                    let (flag, inline) = split_flag(&s);
                    if !try_project_flag(flag, inline, rest, &mut i, &mut project)? {
                        if is_run && flag == "--script" {
                            script = next_str(rest, &mut i, inline, flag)?;
                        } else {
                            bail!("unexpected argument {flag:?} for {first}");
                        }
                    }
                } else {
                    ensure!(project.package.is_none(), "unexpected positional argument: {s}");
                    project.package = Some(rest[i].clone().into_string().map_err(|_| anyhow::anyhow!("non-UTF8 package"))?);
                }
                i += 1;
            }
            let execution = Execution {
                project,
                args: trailing,
            };
            if is_run {
                Ok(Commands::Run { execution, script })
            } else {
                Ok(Commands::Test(execution))
            }
        }
        "export" => {
            let mut project = default_project();
            let mut out = None;
            let mut include_sources = false;
            let mut plan_only = false;
            let mut i = 0;
            while i < rest.len() {
                let s = rest[i].to_string_lossy();
                if s.starts_with('-') && s != "-" {
                    let (flag, inline) = split_flag(&s);
                    if !try_project_flag(flag, inline, rest, &mut i, &mut project)? {
                        match flag {
                            "-o" | "--out" => {
                                out = Some(PathBuf::from(next_val(rest, &mut i, inline, flag)?));
                            }
                            "--include-sources" => include_sources = true,
                            "--plan-only" => plan_only = true,
                            _ => bail!("unexpected argument {flag:?} for export"),
                        }
                    }
                } else {
                    ensure!(project.package.is_none(), "unexpected positional argument: {s}");
                    project.package = Some(rest[i].clone().into_string().map_err(|_| anyhow::anyhow!("non-UTF8 package"))?);
                }
                i += 1;
            }
            Ok(Commands::Export {
                project,
                out,
                include_sources,
                plan_only,
            })
        }
        "project-inputs" => {
            let mut project = default_project();
            let mut sources = None;
            let mut graph = None;
            let mut i = 0;
            while i < rest.len() {
                let s = rest[i].to_string_lossy();
                if s.starts_with('-') && s != "-" {
                    let (flag, inline) = split_flag(&s);
                    if !try_project_flag(flag, inline, rest, &mut i, &mut project)? {
                        match flag {
                            "--sources" => {
                                sources = Some(PathBuf::from(next_val(rest, &mut i, inline, flag)?));
                            }
                            "--graph" => {
                                graph = Some(PathBuf::from(next_val(rest, &mut i, inline, flag)?));
                            }
                            _ => bail!("unexpected argument {flag:?} for project-inputs"),
                        }
                    }
                } else {
                    ensure!(project.package.is_none(), "unexpected positional argument: {s}");
                    project.package = Some(rest[i].clone().into_string().map_err(|_| anyhow::anyhow!("non-UTF8 package"))?);
                }
                i += 1;
            }
            Ok(Commands::ProjectInputs {
                project,
                sources: sources.ok_or_else(|| anyhow::anyhow!("missing --sources"))?,
                graph: graph.ok_or_else(|| anyhow::anyhow!("missing --graph"))?,
            })
        }
        "workspace-info" => {
            let mut directory = None;
            let mut list = false;
            let mut filters = Vec::new();
            let mut after_sep = false;
            for a in rest {
                if !after_sep && a == "--" {
                    after_sep = true;
                    continue;
                }
                if !after_sep && a == "--list" {
                    list = true;
                    continue;
                }
                if directory.is_none() {
                    directory = Some(PathBuf::from(a));
                } else {
                    filters.push(a.clone().into_string().map_err(|_| anyhow::anyhow!("non-UTF8 filter"))?);
                }
            }
            Ok(Commands::WorkspaceInfo {
                directory: directory.ok_or_else(|| anyhow::anyhow!("missing directory"))?,
                list,
                filters,
            })
        }
        "run-script" => {
            let mut project = None;
            let mut pos = Vec::new();
            let mut trailing = Vec::new();
            let mut i = 0;
            while i < rest.len() {
                if rest[i] == "--" {
                    trailing.extend_from_slice(&rest[i + 1..]);
                    break;
                }
                let s = rest[i].to_string_lossy();
                if s.starts_with("--") {
                    let (flag, inline) = split_flag(&s);
                    ensure!(flag == "--project", "unexpected argument {flag:?} for run-script");
                    project = Some(PathBuf::from(next_val(rest, &mut i, inline, flag)?));
                } else {
                    pos.push(rest[i].clone());
                }
                i += 1;
            }
            ensure!(pos.len() == 3, "run-script expects <installation> <importer> <script>");
            Ok(Commands::RunScript {
                installation: PathBuf::from(&pos[0]),
                project,
                importer: PathBuf::from(&pos[1]),
                script: pos[2].clone().into_string().map_err(|_| anyhow::anyhow!("non-UTF8 script"))?,
                args: trailing,
            })
        }
        "fetch" => {
            let mut directory = None;
            let mut npmrc = None;
            let mut all = false;
            let mut system = None;
            let mut all_platforms = false;
            let mut i = 0;
            while i < rest.len() {
                let s = rest[i].to_string_lossy();
                if s.starts_with('-') && s != "-" {
                    let (flag, inline) = split_flag(&s);
                    match flag {
                        "--npmrc" => npmrc = Some(PathBuf::from(next_val(rest, &mut i, inline, flag)?)),
                        "--all" => all = true,
                        "--system" => system = Some(next_str(rest, &mut i, inline, flag)?),
                        "--all-platforms" => all_platforms = true,
                        _ => bail!("unexpected argument {flag:?} for fetch"),
                    }
                } else {
                    ensure!(directory.is_none(), "unexpected positional argument: {s}");
                    directory = Some(PathBuf::from(&rest[i]));
                }
                i += 1;
            }
            ensure!(
                !(system.is_some() && all_platforms),
                "--system cannot be used with --all-platforms"
            );
            Ok(Commands::Fetch {
                directory: directory.ok_or_else(|| anyhow::anyhow!("missing directory"))?,
                npmrc,
                all,
                system,
                all_platforms,
            })
        }
        "verify" => {
            let mut directory = None;
            let mut store = None;
            let mut i = 0;
            while i < rest.len() {
                let s = rest[i].to_string_lossy();
                if s.starts_with('-') && s != "-" {
                    let (flag, inline) = split_flag(&s);
                    ensure!(flag == "--store", "unexpected argument {flag:?} for verify");
                    store = Some(PathBuf::from(next_val(rest, &mut i, inline, flag)?));
                } else {
                    ensure!(directory.is_none(), "unexpected positional argument: {s}");
                    directory = Some(PathBuf::from(&rest[i]));
                }
                i += 1;
            }
            Ok(Commands::Verify {
                directory: directory.ok_or_else(|| anyhow::anyhow!("missing directory"))?,
                store,
            })
        }
        "materialize" => {
            let mut directory = None;
            let mut project = None;
            let mut out = None;
            let mut i = 0;
            while i < rest.len() {
                let s = rest[i].to_string_lossy();
                if s.starts_with('-') && s != "-" {
                    let (flag, inline) = split_flag(&s);
                    match flag {
                        "--project" => project = Some(PathBuf::from(next_val(rest, &mut i, inline, flag)?)),
                        "-o" | "--out" => out = Some(PathBuf::from(next_val(rest, &mut i, inline, flag)?)),
                        _ => bail!("unexpected argument {flag:?} for materialize"),
                    }
                } else {
                    ensure!(directory.is_none(), "unexpected positional argument: {s}");
                    directory = Some(PathBuf::from(&rest[i]));
                }
                i += 1;
            }
            Ok(Commands::Materialize {
                directory: directory.ok_or_else(|| anyhow::anyhow!("missing directory"))?,
                project,
                out: out.ok_or_else(|| anyhow::anyhow!("missing --out"))?,
            })
        }
        "assemble" => {
            let mut pos = Vec::new();
            let mut keep_verification_store = false;
            let mut unpacked_store = None;
            let mut package_contents = None;
            let mut shared_packages = None;
            let mut i = 0;
            while i < rest.len() {
                let s = rest[i].to_string_lossy();
                if s.starts_with("--") {
                    let (flag, inline) = split_flag(&s);
                    match flag {
                        "--keep-verification-store" => keep_verification_store = true,
                        "--unpacked-store" => {
                            unpacked_store = Some(PathBuf::from(next_val(rest, &mut i, inline, flag)?));
                        }
                        "--package-contents" => {
                            package_contents = Some(PathBuf::from(next_val(rest, &mut i, inline, flag)?));
                        }
                        "--shared-packages" => {
                            shared_packages = Some(PathBuf::from(next_val(rest, &mut i, inline, flag)?));
                        }
                        _ => bail!("unexpected argument {flag:?} for assemble"),
                    }
                } else {
                    pos.push(PathBuf::from(&rest[i]));
                }
                i += 1;
            }
            ensure!(pos.len() == 3, "assemble expects <source> <out> <tarballs>");
            Ok(Commands::Assemble {
                source: pos.remove(0),
                out: pos.remove(0),
                tarballs: pos.remove(0),
                keep_verification_store,
                unpacked_store,
                package_contents,
                shared_packages,
            })
        }
        "prepare-package" => {
            ensure!(rest.len() == 4, "prepare-package expects 4 arguments");
            Ok(Commands::PreparePackage {
                archive: PathBuf::from(&rest[0]),
                out: PathBuf::from(&rest[1]),
                id: rest[2].clone().into_string().map_err(|_| anyhow::anyhow!("non-UTF8 id"))?,
                integrity: rest[3].clone().into_string().map_err(|_| anyhow::anyhow!("non-UTF8 integrity"))?,
            })
        }
        "patch-package" => {
            ensure!(rest.len() == 4, "patch-package expects 4 arguments");
            Ok(Commands::PatchPackage {
                source: PathBuf::from(&rest[0]),
                out: PathBuf::from(&rest[1]),
                patch: PathBuf::from(&rest[2]),
                hash: rest[3].clone().into_string().map_err(|_| anyhow::anyhow!("non-UTF8 hash"))?,
            })
        }
        "attach" => {
            ensure!(rest.len() == 3, "attach expects 3 arguments");
            Ok(Commands::Attach {
                source: PathBuf::from(&rest[0]),
                layer: PathBuf::from(&rest[1]),
                out: PathBuf::from(&rest[2]),
            })
        }
        "internal-plan" => Ok(Commands::InternalPlan),
        "internal-symlink" => {
            ensure!(rest.len() == 2, "internal-symlink expects <target> <out>");
            Ok(Commands::InternalSymlink {
                target: PathBuf::from(&rest[0]),
                out: PathBuf::from(&rest[1]),
            })
        }
        "prepare-zstd" => {
            ensure!(rest.len() == 2, "prepare-zstd expects <archive> <out>");
            Ok(Commands::PrepareZstd {
                archive: PathBuf::from(&rest[0]),
                out: PathBuf::from(&rest[1]),
            })
        }
        other => bail!("unknown command: {other}"),
    }
}
