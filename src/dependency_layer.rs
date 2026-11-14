//! Reuse a complete, immutable installation across workspace source edits.
//! The layer keeps the inputs assembly actually reads; attachment restores the
//! full selected sources and relocates generated pnpm metadata and bin shims.
use crate::{native_store as store, workspace};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::symlink,
    path::{Component, Path, PathBuf},
};
use walkdir::WalkDir;

pub fn files(project: &Path) -> Result<BTreeSet<String>> {
    let lock: Value = serde_yaml_ng::from_slice(&fs::read(project.join("pnpm-lock.yaml"))?)?;
    files_for_lock(project, &lock)
}

fn files_for_lock(project: &Path, lock: &Value) -> Result<BTreeSet<String>> {
    let mut paths: Vec<PathBuf> = [
        "pnpm-lock.yaml",
        "pnpm-workspace.yaml",
        ".npmrc",
        ".pnpmfile.cjs",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect();
    for importer in workspace::importer_paths_from_lock(lock)? {
        let path = importer.join("package.json");
        let manifest: Value = serde_json::from_slice(&fs::read(project.join(&path))?)?;
        paths.push(path);
        let bins: Vec<&str> = match &manifest["bin"] {
            Value::String(path) => vec![path],
            Value::Object(bins) => bins.values().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        };
        for bin in bins {
            // Invalid, unused bin declarations were previously ignored. The
            // assembler still rejects them if the package's bins are linked.
            if let Ok(path) = store::safe_relative(bin) {
                paths.push(importer.join(path));
            }
        }
    }
    if let Some(patches) = lock["patchedDependencies"].as_object() {
        for patch in patches.values() {
            paths.push(
                store::safe_relative(patch["path"].as_str().context("missing patch path")?)?.into(),
            );
        }
    }
    paths
        .into_iter()
        // A bin may point into the installation itself. That is generated data,
        // not a source input; classify it the same before and after assembly.
        .filter(|path| {
            !path.components().any(|c| {
                matches!(
                    c.as_os_str().to_str(),
                    Some("node_modules" | ".pnpm-store" | ".git")
                )
            }) && project.join(path).is_file()
        })
        .map(|path| {
            let path: PathBuf = path
                .components()
                .filter(|c| *c != Component::CurDir)
                .collect();
            Ok(path
                .to_str()
                .context("non-UTF8 dependency input")?
                .to_owned())
        })
        .collect()
}

fn validate(source: &Path, layer: &Path) -> Result<Vec<PathBuf>> {
    let mut expected: Value = serde_yaml_ng::from_slice(&fs::read(source.join("pnpm-lock.yaml"))?)?;
    let actual: Value = serde_yaml_ng::from_slice(&fs::read(layer.join("pnpm-lock.yaml"))?)?;
    let paths = files_for_lock(source, &expected)?;
    ensure!(
        paths == files_for_lock(layer, &actual)?,
        "dependency layer input paths differ"
    );
    for path in &paths {
        if path != "pnpm-lock.yaml" {
            ensure!(
                fs::read(source.join(path))? == fs::read(layer.join(path))?,
                "dependency layer input differs: {path}"
            );
        }
    }
    // Assembly can supplement missing optional platform constraints from trusted
    // manifests. These are the only permitted differences from the source lock.
    let required: BTreeSet<String> = expected["snapshots"]
        .as_object()
        .into_iter()
        .flat_map(|snapshots| snapshots.iter())
        .filter(|(_, snapshot)| snapshot["optional"].as_bool() != Some(true))
        .map(|(id, _)| crate::native::base_id(id).to_owned())
        .collect();
    if let Some(packages) = expected.get_mut("packages").and_then(Value::as_object_mut) {
        for (id, metadata) in packages {
            if !required.contains(id.as_str()) {
                for key in ["os", "cpu", "libc"] {
                    if metadata.get(key).is_none()
                        && actual["packages"][id.as_str()][key].is_array()
                    {
                        metadata[key] = actual["packages"][id.as_str()][key].clone();
                    }
                }
            }
        }
    }
    ensure!(expected == actual, "dependency layer lockfile differs");
    workspace::importer_paths_from_lock(&actual)
}

fn copy_installation(source: &Path, target: &Path, old_root: &Path, new_root: &Path) -> Result<()> {
    for entry in WalkDir::new(source).follow_root_links(false) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(source)?;
        let destination = if relative.as_os_str().is_empty() {
            target.to_path_buf()
        } else {
            target.join(relative)
        };
        if entry.file_type().is_dir() {
            fs::create_dir_all(&destination)?;
        } else if entry.file_type().is_symlink() {
            // Native assembly uses relative links, preserving workspace and peer
            // resolution when the entire installation is moved to another root.
            let link = fs::read_link(entry.path())?;
            ensure!(!link.is_absolute(), "absolute link in dependency layer");
            symlink(link, destination)?;
        } else {
            ensure!(
                entry.file_type().is_file(),
                "unsupported dependency layer entry"
            );
            let generated = matches!(
                entry.file_name().to_str(),
                Some(".modules.yaml" | ".pnpm-workspace-state-v1.json")
            ) || entry
                .path()
                .parent()
                .is_some_and(|p| p.file_name().is_some_and(|name| name == ".bin"));
            if generated && let Ok(text) = fs::read_to_string(entry.path()) {
                fs::write(
                    &destination,
                    text.replace(
                        old_root.to_str().context("non-UTF8 layer path")?,
                        new_root.to_str().context("non-UTF8 output path")?,
                    ),
                )?;
                fs::set_permissions(destination, entry.metadata()?.permissions())?;
            } else {
                fs::copy(entry.path(), destination)?;
            }
        }
    }
    Ok(())
}

pub fn attach(source: &Path, layer: &Path, out: &Path) -> Result<()> {
    let started = std::time::Instant::now();
    let layer = layer.canonicalize()?;
    let importers = validate(source, &layer)?;
    ensure!(!out.exists(), "output already exists");
    fs::create_dir(out)?;
    let out = out.canonicalize()?;
    workspace::copy_tree(source, &out)?;
    // Preserve the completed lockfile and executable modes of workspace bins.
    workspace::copy_tree(&layer, &out)?;
    let mut tasks = Vec::new();
    for importer in importers {
        let modules = importer.join("node_modules");
        if !layer.join(&modules).exists() {
            continue;
        }
        fs::create_dir_all(out.join(&modules))?;
        for entry in fs::read_dir(layer.join(&modules))? {
            let entry = entry?;
            let path = modules.join(entry.file_name());
            if entry.file_name() == ".pnpm" && entry.file_type()?.is_dir() {
                fs::create_dir_all(out.join(&path))?;
                for package in fs::read_dir(entry.path())? {
                    tasks.push(path.join(package?.file_name()));
                }
            } else {
                tasks.push(path);
            }
        }
    }
    store::parallel_each(&tasks, |path| {
        copy_installation(&layer.join(path), &out.join(path), &layer, &out)
            .with_context(|| format!("copying dependency layer entry {}", path.display()))
    })?;
    if layer.join(".pnpm-store").exists() {
        copy_installation(
            &layer.join(".pnpm-store"),
            &out.join(".pnpm-store"),
            &layer,
            &out,
        )?;
    }
    eprintln!(
        "drvjs: attached cached dependencies in {:.3}s",
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn installation_copy_keeps_file_and_symlink_roots() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("target");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&target).unwrap();
        fs::write(source.join("file"), "contents").unwrap();
        symlink("file", source.join("link")).unwrap();
        for name in ["file", "link"] {
            copy_installation(&source.join(name), &target.join(name), &source, &target).unwrap();
        }
        assert_eq!(fs::read_to_string(target.join("file")).unwrap(), "contents");
        assert_eq!(
            fs::read_link(target.join("link")).unwrap(),
            Path::new("file")
        );
    }

    #[test]
    fn empty_installation_keeps_the_absence_of_package_metadata() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        store::write_json(&source.join("package.json"), &json!({"name":"empty"})).unwrap();
        let lock = json!({"lockfileVersion":"9.0","importers":{".":{}}});
        store::write_json(&source.join("pnpm-lock.yaml"), &lock).unwrap();
        let layer = root.path().join("layer");
        workspace::copy_tree(&source, &layer).unwrap();
        let out = root.path().join("out");
        attach(&source, &layer, &out).unwrap();
        assert!(!out.join("node_modules").exists());
        assert_eq!(
            fs::read(source.join("pnpm-lock.yaml")).unwrap(),
            fs::read(out.join("pnpm-lock.yaml")).unwrap()
        );
    }

    #[test]
    fn generated_bins_do_not_become_source_inputs_after_installation() {
        let root = tempfile::tempdir().unwrap();
        store::write_json(
            &root.path().join("package.json"),
            &json!({
                "name":"app", "bin":{"tool":"node_modules/dep/bin.js", "unused":"../outside"}
            }),
        )
        .unwrap();
        store::write_json(
            &root.path().join("pnpm-lock.yaml"),
            &json!({"importers":{".":{}}}),
        )
        .unwrap();
        let before = files(root.path()).unwrap();
        fs::create_dir_all(root.path().join("node_modules/dep")).unwrap();
        fs::write(
            root.path().join("node_modules/dep/bin.js"),
            "#!/usr/bin/env node\n",
        )
        .unwrap();
        assert_eq!(before, files(root.path()).unwrap());
    }
}
