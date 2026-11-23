use crate::cli::{CiWorkspace, Lockfiles};
use anyhow::{ensure, Context, Result};
use drvbench_support::*;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    env, fs,
    path::{Component, Path},
    process::{Command, Stdio},
    time::Instant,
};
use walkdir::WalkDir;

pub fn lockfiles(a: Lockfiles) -> Result<()> {
    let modes = [("baseline", &a.baseline), ("optimized", &a.optimized)];
    let mut inputs = BTreeMap::new();
    let mut results = vec![];
    for path in &a.project {
        let project = path.canonicalize()?;
        let name = project
            .file_name()
            .context("project needs a name")?
            .to_str()
            .context("non-UTF-8 project name")?;
        ensure!(
            inputs
                .insert(
                    name.to_string(),
                    sha256(&fs::read(project.join("pnpm-lock.yaml"))?)
                )
                .is_none(),
            "project names must be unique"
        );
        for operation in ["list", "select", "filtered_export", "full_export"] {
            let mut samples = BTreeMap::new();
            let mut expected = None;
            for trial in 0..=a.runs {
                for (mode, bin) in alternating(&modes, trial) {
                    let s = tempfile::tempdir()?;
                    let export = s.path().join("export");
                    let mut cmd = match operation {
                        "list" => argv![bin, "workspace-info", project, "--list"],
                        "select" => argv![bin, "workspace-info", project, "--", a.filter],
                        _ => argv![bin, "export", "-C", project, "-o", export],
                    };
                    if operation == "filtered_export" {
                        cmd.push(a.filter.clone());
                    }
                    let (out, secs) = measured(|| run(&cmd))?;
                    let signature: Value = if export.is_dir() {
                        json!([
                            sha256(&fs::read(export.join("project/pnpm-lock.yaml"))?),
                            sha256(&fs::read(export.join("plan.json"))?)
                        ])
                    } else {
                        serde_json::from_str(&out)?
                    };
                    ensure!(
                        expected.get_or_insert_with(|| signature.clone()) == &signature,
                        "different outputs for {operation} in {}",
                        project.display()
                    );
                    if trial > 0 {
                        samples
                            .entry(mode)
                            .or_insert_with(Vec::new)
                            .push((secs * 1e6).round() / 1000.);
                    }
                }
            }
            let medians = samples
                .iter()
                .map(|(k, v)| Ok((*k, median(v.clone())?)))
                .collect::<Result<BTreeMap<_, _>>>()?;
            results.push(json!({"project":name,"operation":operation,"samples_ms":samples,"median_ms":medians}));
        }
    }
    pretty(
        &json!({"baseline":a.baseline,"optimized":a.optimized,"filter":a.filter,"runs":a.runs,"inputs":inputs,"results":results}),
    )
}
fn checkout(src: &Path, dest: &Path) -> Result<()> {
    for item in WalkDir::new(src).into_iter().filter_entry(|e| {
        ![".git", "node_modules", ".pnpm-store"]
            .iter()
            .any(|n| e.file_name() == *n)
    }) {
        let item = item?;
        let path = item.path();
        let to = dest.join(path.strip_prefix(src)?);
        if item.file_type().is_dir() {
            fs::create_dir_all(&to)?;
        } else if item.file_type().is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(path)?, to)?;
        } else if item.file_type().is_file() {
            fs::copy(path, to)?;
        }
    }
    Ok(())
}
pub fn workspace(a: CiWorkspace) -> Result<()> {
    ensure!(
        a.edit_file
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir)),
        "--edit-file must be project-relative"
    );
    let scratch = tempfile::tempdir()?;
    let root = scratch.path().canonicalize()?;
    let project = root.join("checkout");
    checkout(&a.project, &project)?;
    let edit = project.join(&a.edit_file);
    ensure!(
        edit.canonicalize()? == edit && edit.is_file(),
        "edit file must be a regular file inside the checkout"
    );
    let original = fs::read(&edit)?;
    let modes = [("baseline", &a.baseline), ("optimized", &a.optimized)];
    let mut rows = vec![];
    let mut include_sources = BTreeMap::new();
    for (mode, bin) in modes {
        let needs_flag = !project.join("pnpm-workspace.yaml").exists()
            && run(&args_array![bin, "export", "--help"])?.contains("--include-sources");
        include_sources.insert(mode, needs_flag);
    }
    let ci = |c: &mut Command| {
        c.env("CI", "true").env("DRVJS_JOBS", "4");
    };
    for trial in 0..=a.runs {
        let mut revision = original.clone();
        revision.extend(
            format!(
                "\n// {}-{trial}\n",
                root.file_name().unwrap().to_string_lossy()
            )
            .bytes(),
        );
        fs::write(&edit, &revision)?;
        for (mode, bin) in alternating(&modes, trial) {
            let job = tempfile::tempdir_in(&root)?;
            let path = job.path();
            let export = path.join("export");
            let cache = path.join("cache");
            let work = path.join("work");
            let mut cmd = Command::new(bin);
            cmd.arg("export")
                .arg("-C")
                .arg(&project)
                .arg("--cache-dependencies");
            if include_sources[mode] {
                cmd.arg("--include-sources");
            }
            for f in &a.filter {
                cmd.arg("--filter").arg(f);
            }
            cmd.arg("-o").arg(&export);
            ci(&mut cmd);
            let start = Instant::now();
            quiet(&mut cmd)?;
            let exported = Instant::now();
            let mut cmd = Command::new("nix-build");
            cmd.arg(&export);
            if mode == "optimized" {
                cmd.args(["-A", "dependencies"]);
            }
            cmd.args(["--max-jobs", "4", "--cores", "4", "-o"])
                .arg(&cache);
            ci(&mut cmd);
            quiet(&mut cmd)?;
            let built = Instant::now();
            let mut cmd = Command::new(bin);
            cmd.arg("materialize").arg(&cache);
            if mode == "optimized" {
                cmd.arg("--project").arg(export.join("project"));
            }
            ci(&mut cmd);
            quiet(cmd.arg("-o").arg(&work))?;
            let ready = Instant::now();
            ensure!(
                fs::read(work.join(&a.edit_file))? == revision,
                "materialized revision differs"
            );
            let end = if !a.command.is_empty() {
                let mut cmd = command(&a.command);
                ci(&mut cmd);
                let mut paths = vec![work.join("node_modules/.bin")];
                paths.extend(env::split_paths(&env::var_os("PATH").unwrap_or_default()));
                let status = cmd
                    .current_dir(&work)
                    .env("PATH", env::join_paths(paths)?)
                    .stdout(Stdio::from(std::io::stderr()))
                    .status()?;
                ensure!(status.success(), "CI command failed");
                Instant::now()
            } else {
                ready
            };
            if trial > 0 {
                rows.push(json!({"mode":mode,"trial":trial,"export":(exported-start).as_secs_f64(),"nix":(built-exported).as_secs_f64(),"materialize":(ready-built).as_secs_f64(),"ready":(ready-start).as_secs_f64(),"command":(end-ready).as_secs_f64(),"total":(end-start).as_secs_f64()}));
            }
        }
    }
    let mut medians = json!({});
    for (mode, _) in modes {
        for phase in ["export", "nix", "materialize", "ready", "command", "total"] {
            medians[mode][phase] = json!(stats(rows.iter().filter(|r| r["mode"] == mode), phase)?);
        }
    }
    pretty(
        &json!({"project":a.project,"filters":a.filter,"edit_file":a.edit_file,"command":a.command,"samples":rows,"median_seconds":medians}),
    )
}
