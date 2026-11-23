use crate::cli::{Evaluate, Rebuild};
use anyhow::{ensure, Context, Result};
use drvbench_support::*;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

pub(crate) fn cases(values: &[String]) -> Result<Vec<(String, PathBuf)>> {
    let mut cases = Vec::new();
    for value in values {
        let (name, path) = value.split_once('=').context("expected NAME=EXPORT")?;
        ensure!(
            safe_label(name) && !path.is_empty() && !cases.iter().any(|(n, _)| n == name),
            "invalid or repeated case name {name}"
        );
        cases.push((name.to_owned(), Path::new(path).canonicalize()?));
    }
    Ok(cases)
}
pub(crate) fn build_args(path: &Path) -> Vec<String> {
    argv![
        "nix-build",
        path,
        "--no-out-link",
        "--max-jobs",
        "4",
        "--cores",
        "4"
    ]
}
pub fn evaluate(a: Evaluate) -> Result<()> {
    let modes = [
        ("before", a.before.canonicalize()?),
        ("after", a.after.canonicalize()?),
    ];
    let clients = ["nix", "iets-1", "iets-4"];
    let invoke = |client: &str, p: &Path| -> Result<_> {
        let expr = format!("(import (builtins.toPath {}) {{}}).drvPath", quote_path(p)?);
        let mut cmd = if client == "nix" {
            argv!["nix-instantiate", "--eval", "--strict", "--read-write-mode"]
        } else {
            argv![
                a.iets,
                "eval",
                "--impure",
                "--no-tracing-cache",
                "--workers",
                &client[5..]
            ]
        };
        cmd.extend(argv![
            "--option",
            "allow-import-from-derivation",
            "false",
            "-E",
            expr
        ]);
        let (text, seconds) = measured(|| {
            output(
                command(&cmd)
                    .env("IETS_NIX_FALLBACK", "never")
                    .env("IETS_PROGRESS", "false"),
            )
        })?;
        Ok((serde_json::from_str::<String>(&text)?, seconds, cmd))
    };
    let mut paths = BTreeMap::new();
    for (mode, p) in &modes {
        for client in clients {
            let (path, _, _) = invoke(client, p)?;
            ensure!(
                paths
                    .entry(mode.to_string())
                    .or_insert_with(|| path.clone())
                    == &path,
                "evaluator identity differs"
            );
        }
    }
    let mut rows = vec![];
    for trial in 0..a.runs {
        for client in clients {
            for (mode, p) in alternating(&modes, trial) {
                let (value, seconds, cmd) = invoke(client, &p)?;
                ensure!(paths[mode] == value, "unstable derivation");
                rows.push(json!({"evaluator":client,"mode":mode,"trial":trial,"seconds":seconds,"command":cmd}));
            }
        }
    }
    let mut medians = json!({});
    for client in clients {
        for (mode, _) in &modes {
            medians[client][*mode] = json!(stats(
                rows.iter()
                    .filter(|r| r["mode"] == *mode && r["evaluator"] == client),
                "seconds"
            )?);
        }
    }
    save(
        &a.out,
        &json!({"derivations":paths,"samples":rows,"medians":medians}),
    )
}
pub fn rebuild(a: Rebuild) -> Result<()> {
    ensure!(
        !(a.fresh && a.source_edits),
        "--fresh conflicts with --source-edits"
    );
    let mut cases = if a.case.is_empty() {
        vec![
            (
                "before".into(),
                a.before.context("missing --before")?.canonicalize()?,
            ),
            (
                "after".into(),
                a.after.context("missing --after")?.canonicalize()?,
            ),
        ]
    } else {
        ensure!(
            a.before.is_none() && a.after.is_none(),
            "--case conflicts with --before/--after"
        );
        cases(&a.case)?
    };
    fresh_dir(&a.out)?;
    let root = a.out.canonicalize()?;
    let mut outputs = BTreeMap::new();
    let mut floating = BTreeMap::new();
    for (name, p) in &mut cases {
        if a.source_edits {
            let dest = root.join("exports").join(&*name);
            copy_tree(p, &dest, true)?;
            *p = dest;
        }
        let output = run(&build_args(p))?;
        let info: Value =
            serde_json::from_str(&run(&argv!["nix", "path-info", "--json", &output])?)?;
        floating.insert(name.clone(), info[&output]["ca"].is_string());
        outputs.insert(name.clone(), output);
    }
    let nonce = nonce()?;
    let mut rows = vec![];
    for trial in 0..a.runs {
        let mut order = alternating(&cases, trial);
        let n = order.len();
        if n > 2 {
            order.rotate_left(trial % n);
        }
        for (mode, p) in order {
            let marker = ".drvjs-benchmark-source-edit";
            let content = format!("{nonce}-{trial}\n");
            if a.source_edits {
                fs::write(p.join("project").join(marker), &content)?;
            }
            let mut cmd = build_args(&p);
            if a.fresh || (!a.source_edits && floating[&mode]) {
                cmd = argv![
                    "nix-build",
                    "--expr",
                    include_str!("fresh.nix"),
                    "--argstr",
                    "export",
                    p,
                    "--argstr",
                    "nonce",
                    format!("{nonce}-{mode}-{trial}"),
                    "--arg",
                    "freshDependencies",
                    if a.fresh { "true" } else { "false" },
                    "--no-out-link",
                    "--max-jobs",
                    "4",
                    "--cores",
                    "4"
                ];
            } else if !a.source_edits {
                // Historical input-addressed exports support the classic check.
                cmd.push("--check".into());
            }
            let log = log_file(&root, &format!("{mode}-{trial}.log"))?;
            let (result, seconds) = measured(|| logged(&cmd, &log))?;
            if a.source_edits {
                ensure!(
                    fs::read_to_string(Path::new(&result).join(marker))? == content,
                    "source edit missing"
                );
            } else if !a.fresh {
                ensure!(result == outputs[&mode], "output path changed");
            }
            rows.push(
                json!({"mode":mode,"trial":trial,"seconds":seconds,"command":cmd,"output":result}),
            );
            let mut medians = json!({});
            for (name, _) in &cases {
                let matching = rows
                    .iter()
                    .filter(|r| r["mode"] == *name)
                    .collect::<Vec<&Value>>();
                if !matching.is_empty() {
                    medians[name] = json!(stats(matching, "seconds")?);
                }
            }
            save(
                &root.join("results.json"),
                &json!({"outputs":outputs,"samples":rows,"medians":medians}),
            )?;
        }
    }
    Ok(())
}
