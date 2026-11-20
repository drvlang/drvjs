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

fn realize(project: &Project, graph: &Path, link: &Path, dependencies_only: bool) -> Result<()> {
    if let Some(npmrc) = &project.npmrc {
        crate::fetch::fetch(graph, Some(npmrc), false, None, false)?;
    }
    let mut command = Command::new("nix-build");
    command.arg(graph).arg("--out-link").arg(link);
    if dependencies_only {
        command.args(["-A", "dependencies"]).stdout(Stdio::null());
    }
    run(&mut command, "nix-build")
}

pub(crate) fn build(project: &Project, out: &Path) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let graph = temp.path().join("export");
    let link = project.directory.canonicalize()?.join(out);
    export(project, &graph, true, Some(&link))?;
    realize(project, &graph, &link, false)
}

impl Execution {
    pub fn run(&self, script: &str) -> Result<()> {
        let temp = tempfile::tempdir()?;
        let graph = temp.path().join("export");
        let layer = temp.path().join("dependencies");
        let exported = export(&self.project, &graph, true, None)?;
        realize(&self.project, &graph, &layer, true)?;
        run_script(
            &graph.join("project"),
            &layer,
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
