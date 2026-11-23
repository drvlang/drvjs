use crate::cli::{CacheCompression, CacheUpdates, Closure};
use crate::evaluation::{build_args, cases};
use anyhow::{ensure, Context, Result};
use drvbench_support::*;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};
use url::Url;
fn cache_url(path: &Path, level: i32) -> Result<String> {
    let mut url = Url::from_directory_path(path.canonicalize()?)
        .map_err(|_| anyhow::anyhow!("invalid cache directory"))?;
    url.query_pairs_mut()
        .append_pair("compression", "zstd")
        .append_pair("compression-level", &level.to_string());
    Ok(url.to_string())
}
fn compressed(cache: &Path, path: &str) -> Result<u64> {
    let key = Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.split_once('-'))
        .context("invalid store path")?
        .0;
    let info = fs::read_to_string(cache.join(format!("{key}.narinfo")))?;
    Ok(info
        .lines()
        .find_map(|l| l.strip_prefix("FileSize: "))
        .context("missing FileSize")?
        .parse()?)
}
fn path_info(roots: &[String], recursive: bool) -> Result<BTreeMap<String, Value>> {
    let mut cmd = argv!["nix", "path-info", "--json"];
    if recursive {
        cmd.push("--recursive".into());
    }
    cmd.extend_from_slice(roots);
    let data: Value = serde_json::from_str(&run(&cmd)?)?;
    if let Some(list) = data.as_array() {
        list.iter()
            .map(|p| Ok((p["path"].as_str().context("path")?.to_owned(), p.clone())))
            .collect()
    } else {
        Ok(serde_json::from_value(data)?)
    }
}
fn nar_bytes(info: &BTreeMap<String, Value>) -> Result<u64> {
    info.values()
        .map(|v| v["narSize"].as_u64().context("missing narSize"))
        .sum()
}
pub fn compression(a: CacheCompression) -> Result<()> {
    let output = a.output.canonicalize()?;
    let paths: Vec<String> = run(&args_array!["nix-store", "-qR", output])?
        .lines()
        .map(str::to_string)
        .collect();
    let mut rows = vec![];
    for trial in 0..a.runs {
        for level in alternating(&a.levels, trial) {
            let temp = tempfile::tempdir()?;
            let cmd = argv![
                "nix",
                "copy",
                "--to",
                cache_url(temp.path(), level)?,
                output
            ];
            let (_, secs) = measured(|| run(&cmd))?;
            let bytes = paths
                .iter()
                .map(|p| compressed(temp.path(), p))
                .sum::<Result<u64>>()?;
            let row =
                json!({"trial":trial,"level":level,"closure_bytes":bytes,"publish_seconds":secs});
            println!("{row}");
            rows.push(row);
            save(
                &a.out,
                &json!({"output":output,"closure_paths":paths,"samples":rows}),
            )?;
        }
    }
    Ok(())
}
pub fn closure(a: Closure) -> Result<()> {
    let drv = run(&args_array!["nix-instantiate", a.export.canonicalize()?])?;
    let raw: Value = serde_json::from_str(&run(&args_array!["nix", "derivation", "show", drv])?)?;
    let d = raw
        .get("derivations")
        .unwrap_or(&raw)
        .as_object()
        .and_then(|m| m.values().next())
        .context("derivation")?;
    let inputs = d.get("inputs");
    let srcs = inputs.map(|i| &i["srcs"]).unwrap_or(&d["inputSrcs"]);
    let drvs = inputs.map(|i| &i["drvs"]).unwrap_or(&d["inputDrvs"]);
    let store = |s: &str| {
        if s.starts_with("/nix/store/") {
            s.to_owned()
        } else {
            format!("/nix/store/{s}")
        }
    };
    let mut roots = BTreeSet::new();
    for p in srcs.as_array().context("input sources")? {
        roots.insert(store(p.as_str().context("source")?));
    }
    for (p, v) in drvs.as_object().context("input drvs")? {
        for name in v
            .get("outputs")
            .unwrap_or(v)
            .as_array()
            .context("output names")?
        {
            roots.insert(run(&args_array![
                "nix-store",
                "-q",
                "--binding",
                name.as_str().context("output")?,
                store(p)
            ])?);
        }
    }
    let roots = roots.into_iter().collect::<Vec<_>>();
    let paths = path_info(&roots, true)?;
    let output = run(&args_array!["nix-store", "-q", "--binding", "out", drv])?;
    let cache = tempfile::tempdir()?;
    let mut cmd = argv!["nix", "copy", "--to", cache_url(cache.path(), 3)?];
    cmd.extend(roots.clone());
    cmd.push(output.clone());
    run(&cmd)?;
    let result_info = path_info(std::slice::from_ref(&output), false)?;
    let final_info = result_info.get(&output).context("output missing")?;
    let res = json!({"derivation":drv,"input_roots":roots,"input_paths":paths.len(),"input_nar_bytes":nar_bytes(&paths)?,"output":output,"input_zstd_bytes":paths.keys().map(|p|compressed(cache.path(),p)).sum::<Result<u64>>()?,"output_zstd_bytes":compressed(cache.path(),&output)?,"output_nar_bytes":final_info["narSize"],"output_references":final_info["references"]});
    save(&a.out, &res)?;
    pretty(&res)
}
fn payloads(cache: &Path) -> Result<BTreeMap<PathBuf, u64>> {
    let mut files = BTreeMap::new();
    let dir = cache.join("nar");
    if dir.exists() {
        for e in dir.read_dir()? {
            let e = e?;
            let m = e.metadata()?;
            if m.is_file() {
                files.insert(e.path(), m.len());
            }
        }
    }
    Ok(files)
}
pub fn updates(a: CacheUpdates) -> Result<()> {
    let cases = cases(&a.case)?;
    fresh_dir(&a.out)?;
    let scratch = tempfile::tempdir()?;
    let root = scratch.path().canonicalize()?;
    let exports: BTreeMap<_, _> = cases.iter().cloned().collect();
    for (name, p) in &cases {
        let dir = root.join(name);
        copy_tree(p, &dir.join("export"), true)?;
        fs::create_dir(dir.join("cache"))?;
        run(&build_args(&dir.join("export")))?;
    }
    let mut rows = vec![];
    for trial in 0..=a.runs {
        for (name, _) in alternating(&cases, trial) {
            let dir = root.join(&name);
            let ex = dir.join("export");
            let cache = dir.join("cache");
            let marker = ".drvjs-transfer-benchmark";
            let revision = format!("{}-{trial}\n", root.file_name().unwrap().to_string_lossy());
            if trial > 0 {
                fs::write(ex.join("project").join(marker), &revision)?;
            }
            let cmd = build_args(&ex);
            let log = log_file(&a.out, &format!("{name}-{trial}.log"))?;
            let (output, build) = measured(|| logged(&cmd, &log))?;
            if trial > 0 {
                ensure!(
                    fs::read_to_string(Path::new(&output).join(marker))? == revision,
                    "source update missing"
                );
            }
            let before = payloads(&cache)?;
            let publish = argv!["nix", "copy", "--to", cache_url(&cache, a.level)?, output];
            let (s, seconds) = measured(|| combined(&mut command(&publish), &log))?;
            ensure!(s.success(), "cache publication failed");
            let new: Vec<_> = payloads(&cache)?
                .into_iter()
                .filter(|(p, _)| !before.contains_key(p))
                .collect();
            let info = path_info(std::slice::from_ref(&output), true)?;
            rows.push(json!({"case":name,"trial":trial,"phase":if trial==0{"initial"}else{"source-update"},"output":output,"closure":info.keys().collect::<Vec<_>>(),"closure_paths":info.len(),"closure_nar_bytes":nar_bytes(&info)?,"new_payloads":new.len(),"transferred_bytes":new.iter().map(|(_,n)|n).sum::<u64>(),"build_seconds":build,"publish_seconds":seconds,"build_command":cmd,"publish_command":publish}));
            let mut medians = json!({});
            for (n, _) in &cases {
                let r: Vec<_> = rows
                    .iter()
                    .filter(|r| r["case"] == *n && r["trial"] != 0)
                    .collect();
                if !r.is_empty() {
                    for f in ["transferred_bytes", "build_seconds", "publish_seconds"] {
                        medians[n][f] = json!(stats(r.iter().copied(), f)?);
                    }
                }
            }
            save(
                &a.out.join("results.json"),
                &json!({"exports":exports,"level":a.level,"samples":rows,"source_update_medians":medians}),
            )?;
        }
    }
    Ok(())
}
