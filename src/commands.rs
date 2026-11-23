use crate::{BuildOptions, dependency_layer, export, native_store, workspace};
use anyhow::{Context, Result, ensure};
use clap::Args;
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fmt, fs,
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Args)]
pub(crate) struct Project {
    /// Project or workspace root containing pnpm-lock.yaml.
    #[arg(short = 'C', long, default_value = ".")]
    pub directory: PathBuf,
    /// Exact workspace name or root-relative path (defaults to the whole project).
    package: Option<String>,
    /// Include another workspace package and its dependencies. Repeat for a union.
    #[arg(long = "filter")]
    filters: Vec<String>,
    /// Host-only npm credentials for registry tarballs.
    #[arg(long)]
    pub npmrc: Option<PathBuf>,
    #[command(flatten)]
    pub options: BuildOptions,
}

impl Project {
    pub fn selectors(&self) -> Vec<String> {
        self.package.iter().chain(&self.filters).cloned().collect()
    }
}

#[derive(Args)]
pub(crate) struct Execution {
    #[command(flatten)]
    project: Project,
    /// Arguments passed to the package script after --.
    #[arg(last = true)]
    args: Vec<OsString>,
}

#[derive(Debug)]
pub(crate) struct ChildExit(pub i32);

impl fmt::Display for ChildExit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "child process exited with status {}", self.0)
    }
}

impl std::error::Error for ChildExit {}

fn run(command: &mut Command, name: &str) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("running {name}"))?;
    if !status.success() {
        return Err(ChildExit(
            status
                .code()
                .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)),
        )
        .into());
    }
    Ok(())
}

// Export on the host strips npm credentials and completes private tarball
// integrity before any source enters the Nix store. The recursive planner
// independently resolves the sanitized lockfile and instantiates the build.
fn realize(project: &Project, graph: &Path, link: Option<&Path>, quiet: bool) -> Result<()> {
    if let Some(npmrc) = &project.npmrc {
        crate::fetch::fetch(graph, Some(npmrc), false, None, false)?;
    }
    let mut command = Command::new("nix");
    command.args(["build", "--impure", "--print-out-paths"]);
    if let Some(entry) = std::env::var_os("DRVJS_ENTRY") {
        command.arg("--file").arg(entry);
    } else {
        let local = Path::new(env!("CARGO_MANIFEST_DIR"));
        let flake = if local.join("flake.nix").is_file() {
            format!("git+file://{}", local.display())
        } else {
            "github:drvlang/drvjs".into()
        };
        command.args(["--expr", "{ flake, directory, options }: (builtins.getFlake flake).lib.mkJsPackage ((builtins.fromJSON options) // { system = builtins.currentSystem; src = /. + directory; })", "--argstr", "flake"]).arg(flake);
    }
    let mut options = serde_json::to_value(&project.options)?;
    options["dependenciesOnly"] = quiet.into();
    command
        .args(["--argstr", "directory"])
        .arg(graph.join("project"))
        .args(["--argstr", "options"])
        .arg(serde_json::to_string(&options)?)
        .args(["--option", "allow-import-from-derivation", "false"]);
    if let Some(link) = link {
        command.arg("--out-link").arg(link);
    } else {
        command.arg("--no-link");
    }
    if quiet {
        command.stdout(Stdio::null());
    }
    run(&mut command, "nix build")
}

pub(crate) fn build(project: &Project, out: Option<&Path>) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let graph = temp.path().join("export");
    let root = project.directory.canonicalize()?;
    let link = out.map(|o| root.join(o));
    export(project, &graph, true, link.as_deref(), false)?;
    realize(project, &graph, link.as_deref(), false)
}

impl Execution {
    pub fn run(&self, script: &str) -> Result<()> {
        let temp = tempfile::tempdir()?;
        let graph = temp.path().join("export");
        let installation = temp.path().join("installation");
        let exported = export(&self.project, &graph, true, None, false)?;
        realize(&self.project, &graph, Some(&installation), true)?;
        run_script(
            &graph.join("project"),
            &installation,
            Path::new(&exported.primary),
            script,
            &self.args,
        )
    }
}

pub(crate) fn run_script(
    source: &Path,
    layer: &Path,
    importer: &Path,
    script: &str,
    args: &[OsString],
) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let work = temp.path().join("project");
    dependency_layer::materialize(source, layer, &work)?;
    let directory = work.join(native_store::safe_relative(
        importer.to_str().context("invalid workspace path")?,
    )?);
    let manifest: Value = serde_json::from_slice(&fs::read(directory.join("package.json"))?)?;
    ensure!(
        manifest["scripts"][script].is_string(),
        "missing package script {script:?} in {}/package.json",
        importer.display()
    );
    run(
        Command::new("pnpm")
            .current_dir(directory)
            .args([
                "--config.manage-package-manager-versions=false",
                "--silent",
                "run",
                script,
            ])
            .args(args),
        "pnpm",
    )
}

pub(crate) fn workspace_info(root: &Path, list: bool, filters: &[String]) -> Result<()> {
    let info = if list {
        json!(workspace::importer_paths(root)?)
    } else {
        let selection = workspace::select(root, &workspace::read_lock(root)?, filters)?;
        let patches = selection.lock["patchedDependencies"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(_, patch)| {
                native_store::safe_relative(patch["path"].as_str().context("missing patch path")?)
            })
            .collect::<Result<Vec<_>>>()?;
        json!({ "selected": selection.importers, "primary": selection.primary, "patches": patches })
    };
    serde_json::to_writer(std::io::stdout().lock(), &info)?;
    Ok(())
}
