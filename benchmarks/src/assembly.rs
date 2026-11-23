use crate::cli::*;
use anyhow::{bail, ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use bstr::ByteSlice;
use drvbench_support::*;
use flate2::read::MultiGzDecoder;
use rayon::prelude::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256, Sha384, Sha512};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    os::unix::{ffi::OsStrExt, fs::PermissionsExt, process::ExitStatusExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Instant,
};
use walkdir::WalkDir;

#[derive(PartialEq)]
enum Entry {
    Link(PathBuf),
    File(bool, u64),
}
fn entries(root: &Path) -> Result<BTreeMap<PathBuf, Entry>> {
    let mut result = BTreeMap::new();
    for item in WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| e.file_name() != ".git" && e.file_name() != ".pnpm-store")
    {
        let item = item?;
        let path = item.path();
        let rel = path.strip_prefix(root)?;
        if !rel.iter().any(|s| s == "node_modules") {
            continue;
        }
        let kind = item.file_type();
        let entry = if kind.is_symlink() {
            Entry::Link(fs::read_link(path)?)
        } else if kind.is_file() {
            let m = item.metadata()?;
            Entry::File(m.permissions().mode() & 0o111 != 0, m.len())
        } else {
            continue;
        };
        result.insert(rel.to_owned(), entry);
    }
    Ok(result)
}
pub fn compare(a: CompareNative) -> Result<()> {
    let left = a.reference.canonicalize()?;
    let right = a.candidate.canonicalize()?;
    let x = entries(&left)?;
    let y = entries(&right)?;
    let mut diff = Vec::new();
    for key in x.keys().chain(y.keys()).collect::<BTreeSet<_>>() {
        let equal = match (x.get(key), y.get(key)) {
            (Some(Entry::Link(a)), Some(Entry::Link(b))) => a == b,
            (Some(Entry::File(xe, xs)), Some(Entry::File(ye, ys))) if xe == ye => {
                let normalize = key.iter().any(|c| c == ".bin")
                    || key.file_name().is_some_and(|n| {
                        n == ".modules.yaml" || n == ".pnpm-workspace-state-v1.json"
                    });
                if normalize {
                    let normalized = |p: &Path| -> Result<Vec<u8>> {
                        Ok(fs::read(p.join(key))?
                            .as_slice()
                            .replace(p.as_os_str().as_bytes(), b"<ROOT>"))
                    };
                    normalized(&left)? == normalized(&right)?
                } else if xs != ys {
                    false
                } else {
                    let mut a = fs::File::open(left.join(key))?;
                    let mut b = fs::File::open(right.join(key))?;
                    let (mut ab, mut bb) = (vec![0; 1 << 20], vec![0; 1 << 20]);
                    let mut remain = *xs;
                    let mut ok = true;
                    while remain > 0 {
                        let n = remain.min(ab.len() as u64) as usize;
                        a.read_exact(&mut ab[..n])?;
                        b.read_exact(&mut bb[..n])?;
                        if ab[..n] != bb[..n] {
                            ok = false;
                            break;
                        }
                        remain -= n as u64;
                    }
                    ok
                }
            }
            _ => false,
        };
        if !equal {
            diff.push(key);
        }
    }
    pretty(
        &json!({"entries":x.len(),"differences":diff.len(),"first_differences":&diff[..diff.len().min(10)]}),
    )?;
    ensure!(diff.is_empty(), "installed trees differ");
    Ok(())
}
fn record(out: &Path, rows: &mut Vec<Value>, row: Value) -> Result<()> {
    println!("{row}");
    rows.push(row);
    save(&out.join("results.json"), &json!(rows))
}
pub fn materialize(a: Materialize) -> Result<()> {
    ensure!(
        a.dependency_layer.is_none() || a.optimized_arg.is_empty(),
        "--dependency-layer conflicts with --optimized-arg"
    );
    fresh_dir(&a.out)?;
    let mut rows = Vec::new();
    let src = a.source.canonicalize()?;
    let tb = a.tarballs.canonicalize()?;
    for trial in 0..a.runs {
        for mode in alternating(&["baseline", "optimized"], trial) {
            let dir = tempfile::tempdir_in(&a.out)?;
            let dest = dir.path().join("output");
            let binary = if mode == "baseline" {
                &a.baseline
            } else {
                &a.optimized
            };
            let mut args = argv![binary, "assemble", src, dest, tb];
            if mode == "optimized" {
                if let Some(layer) = &a.dependency_layer {
                    args = argv![binary, "attach", src, layer.canonicalize()?, dest];
                } else {
                    args.extend(a.optimized_arg.clone());
                }
            } else {
                args.extend(a.baseline_arg.clone());
            }
            let log = log_file(&a.out, &format!("{mode}-{trial}.log"))?;
            let (status, seconds) = measured(|| combined(&mut command(&args), &log))?;
            let code = status
                .code()
                .unwrap_or_else(|| -status.signal().unwrap_or(1));
            record(
                &a.out,
                &mut rows,
                json!({"mode":mode,"trial":trial,"seconds":seconds,"exit_code":code,"command":args}),
            )?;
            ensure!(status.success(), "assembly exited {status}");
        }
    }
    Ok(())
}
pub fn unpacked(a: UnpackedStore) -> Result<()> {
    fresh_dir(&a.out)?;
    let mut rows = vec![];
    let (tool, src, tb, store) = (
        a.drvjs.canonicalize()?,
        a.source.canonicalize()?,
        a.tarballs.canonicalize()?,
        a.store.canonicalize()?,
    );
    for trial in 0..a.runs {
        for mode in alternating(&["tarballs", "unpacked"], trial) {
            let dir = tempfile::tempdir_in(&a.out)?;
            let mut cmd = argv![tool, "assemble", src, dir.path().join("output"), tb];
            if mode == "unpacked" {
                cmd.extend(argv!["--unpacked-store", store]);
            }
            let log = log_file(&a.out, &format!("{mode}-{trial}.log"))?;
            let (status, seconds) = measured(|| combined(&mut command(&cmd), &log))?;
            ensure!(status.success(), "assembly exited {status}");
            record(
                &a.out,
                &mut rows,
                json!({"mode":mode,"trial":trial,"seconds":seconds}),
            )?;
        }
    }
    Ok(())
}
fn rusage() -> Result<libc::rusage> {
    let mut usage = std::mem::MaybeUninit::uninit();
    // getrusage initializes the complete structure on success.
    if unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, usage.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { usage.assume_init() })
}
fn cpu(t: libc::timeval) -> f64 {
    t.tv_sec as f64 + t.tv_usec as f64 / 1e6
}
pub fn experiment(a: Experiment) -> Result<()> {
    let cases = read_json(&a.cases)?;
    let cases = cases.as_array().context("expected cases array")?;
    fresh_dir(&a.out)?;
    let mut rows = vec![];
    let pressure = || {
        fs::read_to_string("/proc/pressure/io")
            .ok()
            .map(|t| t.trim().to_string())
    };
    for trial in 0..a.runs {
        for c in alternating(cases, trial) {
            let name = c["name"].as_str().context("case name")?;
            ensure!(safe_label(name), "invalid case name");
            let binary = c["binary"].as_str().context("binary")?;
            let scratch = tempfile::tempdir_in(&a.out)?;
            let mut cmd = Command::new(binary);
            cmd.arg("assemble");
            if let Some(v) = c.get("args") {
                for s in v.as_array().context("case args")? {
                    cmd.arg(s.as_str().context("expected string")?);
                }
            }
            let tb = c["tarballs"].as_str().map(Path::new).unwrap_or(&a.tarballs);
            cmd.arg(&a.source)
                .arg(scratch.path().join("output"))
                .arg(tb);
            let env = c.get("env").cloned().unwrap_or(json!({}));
            for (k, v) in env.as_object().context("case env")? {
                cmd.env(k, v.as_str().context("env value")?);
            }
            let before = rusage()?;
            let psi = pressure();
            let log = log_file(&a.out, &format!("{name}-{trial}.log"))?;
            let (status, seconds) = measured(|| combined(&mut cmd, &log))?;
            let after = rusage()?;
            record(
                &a.out,
                &mut rows,
                json!({"case":name,"trial":trial,"seconds":seconds,"user_seconds":cpu(after.ru_utime)-cpu(before.ru_utime),"system_seconds":cpu(after.ru_stime)-cpu(before.ru_stime),"minor_faults":after.ru_minflt-before.ru_minflt,"major_faults":after.ru_majflt-before.ru_majflt,"exit_code":status.code().unwrap_or_else(||-status.signal().unwrap_or(1)),"binary":binary,"tarballs":tb,"io_pressure_before":psi,"io_pressure_after":pressure(),"env":env}),
            )?;
            ensure!(status.success(), "case {name} exited {status}");
        }
    }
    Ok(())
}
pub fn prepare(a: PrepareZstd) -> Result<()> {
    let plan = read_json(&a.plan)?;
    let sources = read_json(&a.tarballs)?;
    let mut hashes = BTreeMap::new();
    for p in plan["packages"].as_array().context("plan packages")? {
        hashes.insert(
            p["id"].as_str().context("id")?,
            p["hash"].as_str().context("hash")?,
        );
    }
    fresh_dir(&a.out)?;
    let out = a.out.canonicalize()?;
    let started = Instant::now();
    let sources = sources.as_array().context("tarballs list")?;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(a.jobs)
        .build()?;
    let rows = pool.install(|| {
        sources
            .par_iter()
            .map(|s| -> Result<_> {
                let id = s["id"].as_str().context("id")?;
                let integrity = hashes.get(id).context("package absent from plan")?;
                let (algo, expected) = integrity.split_once('-').context("invalid integrity")?;
                let data = fs::read(s["file"].as_str().context("file")?)?;
                let digest = match algo {
                    "sha512" => Sha512::digest(&data).to_vec(),
                    "sha384" => Sha384::digest(&data).to_vec(),
                    "sha256" => Sha256::digest(&data).to_vec(),
                    "sha1" => sha1::Sha1::digest(&data).to_vec(),
                    _ => bail!("unsupported integrity {algo}"),
                };
                ensure!(
                    digest == STANDARD.decode(expected)?,
                    "source integrity mismatch: {id}"
                );
                let mut raw = Vec::new();
                MultiGzDecoder::new(&data[..]).read_to_end(&mut raw)?;
                let file = out.join(format!("{}.tar.zst", sha256(id.as_bytes())));
                let mut child = Command::new(&a.zstd)
                    .args(["-q", "-1", "-c", &format!("--stream-size={}", raw.len())])
                    .stdin(Stdio::piped())
                    .stdout(fs::File::create(&file)?)
                    .spawn()?;
                let written = child.stdin.take().context("zstd stdin")?.write_all(&raw);
                let status = child.wait()?;
                written?;
                ensure!(status.success(), "zstd failed");
                let compressed = fs::read(&file)?;
                let mut entry = s.clone();
                entry["file"] = json!(file);
                entry["sourceIntegrity"] = json!(integrity);
                entry["archiveIntegrity"] = json!(format!(
                    "sha512-{}",
                    STANDARD.encode(Sha512::digest(&compressed))
                ));
                Ok((entry, data.len(), raw.len(), compressed.len()))
            })
            .collect::<Result<Vec<_>>>()
    })?;
    save(
        &out.join("manifest.json"),
        &json!(rows.iter().map(|r| &r.0).collect::<Vec<_>>()),
    )?;
    let metrics = json!({"seconds":started.elapsed().as_secs_f64(),"archives":rows.len(),"gzip_bytes":rows.iter().map(|r|r.1).sum::<usize>(),"raw_bytes":rows.iter().map(|r|r.2).sum::<usize>(),"zstd_bytes":rows.iter().map(|r|r.3).sum::<usize>()});
    save(&out.join("preparation.json"), &metrics)?;
    pretty(&metrics)
}
