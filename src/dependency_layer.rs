//! Reuse a complete, immutable installation across workspace source edits.
//! The layer keeps the inputs assembly actually reads; attachment restores the
//! full selected sources and relocates generated pnpm metadata and bin shims.
use crate::{native_store as store, workspace};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Component, Path, PathBuf},
};
use walkdir::WalkDir;

pub fn files_for_lock(project: &Path, lock: &Value) -> Result<BTreeSet<String>> {
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

struct LayerInputs {
    importers: Vec<PathBuf>,
    overlay: BTreeSet<String>,
}

fn validate(source: &Path, layer: &Path) -> Result<LayerInputs> {
    let mut expected: Value = serde_yaml_ng::from_slice(&fs::read(source.join("pnpm-lock.yaml"))?)?;
    let paths = files_for_lock(source, &expected)?;
    let importers = workspace::importer_paths_from_lock(&expected)?;
    if source == layer {
        return Ok(LayerInputs {
            importers,
            overlay: BTreeSet::new(),
        });
    }
    let actual: Value = serde_yaml_ng::from_slice(&fs::read(layer.join("pnpm-lock.yaml"))?)?;
    ensure!(
        paths == files_for_lock(layer, &actual)?,
        "dependency layer input paths differ"
    );
    for path in &paths {
        let input = layer.join(path);
        ensure!(
            input.canonicalize()? == input,
            "unexpected symlink in layer input: {path}"
        );
        if path != "pnpm-lock.yaml" {
            ensure!(
                fs::read(source.join(path))? == fs::read(input)?,
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
    Ok(LayerInputs {
        importers,
        overlay: paths,
    })
}

pub(crate) fn copy_installation(
    source: &Path,
    target: &Path,
    old_root: &Path,
    new_root: &Path,
    shared: Option<&Path>,
    writable: bool,
) -> Result<()> {
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
            ensure!(
                !link.is_absolute() || shared.is_some_and(|root| link.starts_with(root)),
                "unexpected absolute link in dependency layer"
            );
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
                let mut permissions = entry.metadata()?.permissions();
                if writable {
                    permissions.set_mode(permissions.mode() | 0o200);
                }
                fs::set_permissions(destination, permissions)?;
            } else if writable {
                workspace::copy_writable(entry.path(), &destination)?;
            } else {
                fs::copy(entry.path(), destination)?;
            }
        }
    }
    Ok(())
}

pub fn attach(source: &Path, layer: &Path, out: &Path) -> Result<()> {
    restore(source, layer, out, true)
}

pub fn materialize(source: &Path, layer: &Path, out: &Path) -> Result<()> {
    restore(source, layer, out, false)
}

fn restore(source: &Path, layer: &Path, out: &Path, share: bool) -> Result<()> {
    let started = std::time::Instant::now();
    let source = source.canonicalize()?;
    let layer = layer.canonicalize()?;
    let inputs = validate(&source, &layer)?;
    let shared = crate::shared_store::root(&layer)?;
    ensure!(!out.exists(), "output already exists");
    let parent = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let destination = parent
        .canonicalize()?
        .join(out.file_name().context("output has no filename")?);
    // Generated pnpm shims embed NODE_PATH in double quotes, and metadata
    // embeds the root in JSON strings. Nix store paths already obey this;
    // reject unrepresentable public materialize destinations before copying.
    ensure!(
        !destination
            .to_str()
            .context("non-UTF8 output path")?
            .contains(['"', '\\', '$', '`', '\n', '\r', ':']),
        "output path contains characters unsupported by bin shims: quotes, backslashes, dollar signs, backticks, newlines or colons"
    );
    ensure!(
        !destination.starts_with(&source)
            && !destination.starts_with(&layer)
            && shared
                .as_ref()
                .is_none_or(|root| !destination.starts_with(root)),
        "output must be outside the source and dependency inputs"
    );
    fs::create_dir(&destination)?;
    if let Err(err) = populate(
        &source,
        &layer,
        &destination,
        shared.as_deref(),
        share,
        inputs,
    ) {
        fs::remove_dir_all(&destination)
            .with_context(|| format!("removing incomplete output after {err:#}"))?;
        return Err(err);
    }
    eprintln!(
        "drvjs: attached cached dependencies in {:.3}s",
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

fn populate(
    source: &Path,
    layer: &Path,
    out: &Path,
    shared: Option<&Path>,
    share: bool,
    inputs: LayerInputs,
) -> Result<()> {
    workspace::copy_tree(source, out)?;
    // Assembly can complete the lockfile and make workspace bins executable.
    // Ordinary sources always belong to the current project, even when the
    // caller supplies a complete installation from an earlier revision.
    for path in &inputs.overlay {
        workspace::copy_writable(&layer.join(path), &out.join(path))?;
    }
    let mut tasks = Vec::new();
    for importer in inputs.importers {
        let modules = if importer == Path::new(".") {
            PathBuf::from("node_modules")
        } else {
            importer.join("node_modules")
        };
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
        if !share && path == Path::new(crate::shared_store::MARKER) {
            return Ok(());
        }
        if !share
            && let Some(shared) = shared
            && let Some(relative) = path.strip_prefix("node_modules").ok()
            && let Some(target) =
                crate::shared_store::instance(&layer.join(path), relative, shared)?
        {
            return copy_installation(&target, &out.join(path), shared, out, None, true);
        }
        copy_installation(
            &layer.join(path),
            &out.join(path),
            layer,
            out,
            shared,
            !share,
        )
        .with_context(|| format!("copying dependency layer entry {}", path.display()))
    })?;
    if layer.join(".pnpm-store").exists() {
        copy_installation(
            &layer.join(".pnpm-store"),
            &out.join(".pnpm-store"),
            layer,
            out,
            None,
            !share,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn installation_copy_keeps_file_and_symlink_roots() {
        let root = crate::temp::tempdir().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("target");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&target).unwrap();
        fs::write(source.join("file"), "contents").unwrap();
        symlink("file", source.join("link")).unwrap();
        for name in ["file", "link"] {
            copy_installation(
                &source.join(name),
                &target.join(name),
                &source,
                &target,
                None,
                false,
            )
            .unwrap();
        }
        assert_eq!(fs::read_to_string(target.join("file")).unwrap(), "contents");
        assert_eq!(
            fs::read_link(target.join("link")).unwrap(),
            Path::new("file")
        );
    }

    #[test]
    fn empty_installation_keeps_the_absence_of_package_metadata() {
        let root = crate::temp::tempdir().unwrap();
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
        let nested = source.join("nested");
        assert!(
            materialize(&source, &source, &nested)
                .unwrap_err()
                .to_string()
                .contains("outside")
        );
        assert!(!nested.exists());
        let alias = root.path().join("alias");
        symlink(&source, &alias).unwrap();
        assert!(materialize(&source, &source, &alias.join("nested")).is_err());
        assert!(!nested.exists());
        let unsafe_path = root.path().join("$expansion");
        assert!(materialize(&source, &source, &unsafe_path).is_err());
        assert!(!unsafe_path.exists());
    }

    #[test]
    fn generated_bins_do_not_become_source_inputs_after_installation() {
        let root = crate::temp::tempdir().unwrap();
        store::write_json(
            &root.path().join("package.json"),
            &json!({
                "name":"app", "bin":{"tool":"node_modules/dep/bin.js", "unused":"../outside"}
            }),
        )
        .unwrap();
        let lock = json!({"importers":{".":{}}});
        store::write_json(&root.path().join("pnpm-lock.yaml"), &lock).unwrap();
        let before = files_for_lock(root.path(), &lock).unwrap();
        fs::create_dir_all(root.path().join("node_modules/dep")).unwrap();
        fs::write(
            root.path().join("node_modules/dep/bin.js"),
            "#!/usr/bin/env node\n",
        )
        .unwrap();
        assert_eq!(before, files_for_lock(root.path(), &lock).unwrap());
    }

    #[test]
    fn materializing_a_failed_copy_leaves_the_destination_available_for_retry() {
        let tmp = crate::temp::tempdir().unwrap();
        let source = tmp.path().join("project");
        let layer = tmp.path().join("layer");
        let out = tmp.path().join("work");
        fs::create_dir(&source).unwrap();
        store::write_json(&source.join("package.json"), &json!({"name":"app"})).unwrap();
        store::write_json(
            &source.join("pnpm-lock.yaml"),
            &json!({"importers":{".":{}}}),
        )
        .unwrap();
        workspace::copy_tree(&source, &layer).unwrap();
        fs::write(layer.join("deleted.js"), "old revision").unwrap();
        symlink("package.json", source.join("unsupported")).unwrap();
        assert!(materialize(&source, &layer, &out).is_err());
        assert!(!out.exists());
        fs::remove_file(source.join("unsupported")).unwrap();
        materialize(&source, &layer, &out).unwrap();
        assert!(!out.join("deleted.js").exists());
    }
}
