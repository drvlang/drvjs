//! Split immutable registry packages from workspace sources. Node resolves real
//! package paths inside this graph, so it must contain its registry hoists too.
//! Workspace links stay in the project: implicit registry-to-workspace lookups
//! are deliberately unavailable in this layout, as are writes to dependencies.
use crate::{dependency_layer, native, native_store as store};
use anyhow::{Context, Result, ensure};
use serde_json::json;
use std::{
    fs,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

pub const MARKER: &str = "node_modules/.drvjs-shared.json";
const VIRTUAL: &str = "node_modules/.pnpm";

pub fn root(project: &Path) -> Result<Option<PathBuf>> {
    let path = project.join(MARKER);
    if !path.exists() {
        return Ok(None);
    }
    let data: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
    ensure!(data["version"] == 1, "unsupported shared dependency format");
    let root = PathBuf::from(
        data["packages"]
            .as_str()
            .context("missing shared packages path")?,
    );
    ensure!(
        root.is_absolute() && root.join(VIRTUAL).is_dir(),
        "invalid shared packages path"
    );
    Ok(Some(root))
}

/// Only instance-directory links emitted by split() may be expanded. Never
/// follow arbitrary package links, dependency cycles, or workspace links.
pub fn instance(path: &Path, relative: &Path, shared: &Path) -> Result<Option<PathBuf>> {
    if relative.parent() != Some(Path::new(".pnpm")) || !path.is_symlink() {
        return Ok(None);
    }
    let target = fs::read_link(path)?;
    let expected = shared.join("node_modules").join(relative);
    ensure!(
        target == expected && target.is_dir(),
        "invalid shared package link: {}",
        path.display()
    );
    Ok(Some(target))
}

fn aliases(source: &Path, target: &Path, virtual_store: &Path) -> Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        if entry.file_type()?.is_symlink() {
            let link = fs::read_link(entry.path())?;
            let resolved = native::normalized(&entry.path().parent().unwrap().join(&link));
            if resolved.starts_with(virtual_store) {
                fs::create_dir_all(target)?;
                symlink(link, target.join(name))?;
            }
        } else if name.to_string_lossy().starts_with('@') && entry.file_type()?.is_dir() {
            aliases(&entry.path(), &target.join(name), virtual_store)?;
        }
    }
    Ok(())
}

pub fn split(layer: &Path, packages: &Path) -> Result<()> {
    let layer = layer.canonicalize()?;
    ensure!(!packages.exists(), "shared packages output already exists");
    let virtual_store = layer.join(VIRTUAL);
    // Check before moving anything. A registry package tied directly to mutable
    // workspace sources cannot be shared independently of those sources.
    if virtual_store.exists() {
        for entry in WalkDir::new(&virtual_store) {
            let entry = entry?;
            if entry.file_type().is_symlink()
                && !entry.path().starts_with(virtual_store.join("node_modules"))
            {
                let target = native::normalized(
                    &entry
                        .path()
                        .parent()
                        .unwrap()
                        .join(fs::read_link(entry.path())?),
                );
                ensure!(
                    target.starts_with(&virtual_store),
                    "shared dependencies cannot contain registry-to-workspace links: {}; use the standalone layout",
                    entry.path().display()
                );
            }
        }
    }
    fs::create_dir_all(packages.join("node_modules"))?;
    let packages = packages.canonicalize()?;
    let shared_virtual = packages.join(VIRTUAL);
    if !virtual_store.exists() {
        fs::create_dir_all(&shared_virtual)?;
        // An empty project needs no references to the unused second output.
        return Ok(());
    }
    aliases(
        &layer.join("node_modules"),
        &packages.join("node_modules"),
        &virtual_store,
    )?;
    fs::rename(&virtual_store, &shared_virtual)?;
    fs::create_dir_all(&virtual_store)?;
    for entry in fs::read_dir(&shared_virtual)? {
        let entry = entry?;
        let destination = virtual_store.join(entry.file_name());
        if entry.file_type()?.is_dir() && entry.file_name() != "node_modules" {
            symlink(entry.path(), destination)?;
        } else {
            dependency_layer::copy_installation(
                &entry.path(),
                &destination,
                &layer,
                &layer,
                None,
                false,
            )?;
        }
    }
    // Keep the workspace's original hoists in the attachment metadata, but no
    // partial workspace manifests in the immutable graph. Such imports must
    // resolve from project code, not silently see stale filtered sources.
    let hoisted = shared_virtual.join("node_modules");
    if hoisted.exists() {
        for entry in WalkDir::new(&hoisted) {
            let entry = entry?;
            if entry.file_type().is_symlink() {
                let target = native::normalized(
                    &entry
                        .path()
                        .parent()
                        .unwrap()
                        .join(fs::read_link(entry.path())?),
                );
                if !target.starts_with(&shared_virtual) {
                    fs::remove_file(entry.path())?;
                }
            }
        }
    }
    for entry in WalkDir::new(&shared_virtual) {
        let entry = entry?;
        if entry.file_type().is_file()
            && entry
                .path()
                .parent()
                .is_some_and(|p| p.file_name().is_some_and(|n| n == ".bin"))
            && let Ok(content) = fs::read_to_string(entry.path())
            && content.contains(layer.to_str().context("non-UTF8 layer")?)
        {
            fs::write(
                entry.path(),
                content.replace(
                    layer.to_str().context("non-UTF8 layer")?,
                    packages.to_str().context("non-UTF8 shared packages")?,
                ),
            )?;
        }
    }
    store::write_json(
        &layer.join(MARKER),
        &json!({"version":1,"packages":packages}),
    )?;
    eprintln!("drvjs: split registry graph into {}", packages.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separates_registry_aliases_from_workspace_hoists_and_relocates_bins() {
        let tmp = crate::temp::tempdir().unwrap();
        let layer = tmp.path().join("layer");
        let graph = tmp.path().join("graph");
        let package = layer.join(VIRTUAL).join("example@1/node_modules/example");
        fs::create_dir_all(&package).unwrap();
        fs::write(package.join("index.js"), "module.exports=42").unwrap();
        let hoists = layer.join(VIRTUAL).join("node_modules");
        fs::create_dir_all(hoists.join(".bin")).unwrap();
        fs::create_dir_all(layer.join("packages/workspace")).unwrap();
        symlink("../../../packages/workspace", hoists.join("workspace")).unwrap();
        symlink("../example@1/node_modules/example", hoists.join("example")).unwrap();
        symlink(
            ".pnpm/example@1/node_modules/example",
            layer.join("node_modules/alias"),
        )
        .unwrap();
        symlink(
            "../packages/workspace",
            layer.join("node_modules/workspace"),
        )
        .unwrap();
        fs::write(
            hoists.join(".bin/example"),
            format!("NODE_PATH={}/node_modules", layer.display()),
        )
        .unwrap();
        fs::write(hoists.join(".bin/native"), [0xff, 0x00, 0x01]).unwrap();
        let bundled = package.join("node_modules/.bin/bundled");
        fs::create_dir_all(bundled.parent().unwrap()).unwrap();
        fs::write(&bundled, "#!/bin/sh\nexit 0\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&bundled, fs::Permissions::from_mode(0o555)).unwrap();
        split(&layer, &graph).unwrap();
        assert_eq!(root(&layer).unwrap(), Some(graph.clone()));
        assert!(package.join("index.js").is_file());
        assert_eq!(fs::read_to_string(&bundled).unwrap(), "#!/bin/sh\nexit 0\n");
        assert!(layer.join(VIRTUAL).join("example@1").is_symlink());
        assert!(graph.join("node_modules/alias/index.js").is_file());
        assert!(!graph.join("node_modules/workspace").is_symlink());
        assert!(
            !graph
                .join(VIRTUAL)
                .join("node_modules/workspace")
                .is_symlink()
        );
        assert!(hoists.join("workspace").is_symlink());
        assert_eq!(
            fs::read(graph.join(VIRTUAL).join("node_modules/.bin/native")).unwrap(),
            [0xff, 0x00, 0x01]
        );
        assert_eq!(
            fs::read_to_string(graph.join(VIRTUAL).join("node_modules/.bin/example")).unwrap(),
            format!("NODE_PATH={}/node_modules", graph.display())
        );
        assert_eq!(
            fs::read_to_string(hoists.join(".bin/example")).unwrap(),
            format!("NODE_PATH={}/node_modules", layer.display())
        );
    }

    #[test]
    fn rejects_source_bound_registry_links_before_moving_packages() {
        let tmp = crate::temp::tempdir().unwrap();
        let layer = tmp.path().join("layer");
        let graph = tmp.path().join("graph");
        let package = layer.join(VIRTUAL).join("example@1/node_modules");
        fs::create_dir_all(&package).unwrap();
        symlink("../../../../packages/workspace", package.join("workspace")).unwrap();
        assert!(
            split(&layer, &graph)
                .unwrap_err()
                .to_string()
                .contains("registry-to-workspace")
        );
        assert!(package.is_dir());
        assert!(!graph.exists());
    }

    #[test]
    fn expansion_rejects_redirected_instance_roots() {
        let tmp = crate::temp::tempdir().unwrap();
        let graph = tmp.path().join("graph");
        let path = tmp.path().join("link");
        symlink(tmp.path(), &path).unwrap();
        assert!(instance(&path, Path::new(".pnpm/example@1"), &graph).is_err());
        assert!(
            instance(&path, Path::new("workspace"), &graph)
                .unwrap()
                .is_none()
        );
    }
}
