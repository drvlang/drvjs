use crate::hash::{Sha256, base64_decode, hex_encode};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, PartialEq)]
pub struct Package {
    pub id: String,
    pub path: String,
    pub url: String,
    pub hash: String,
    pub name: String,
    pub private: bool,
}

impl Package {
    pub fn to_value(&self) -> Value {
        json!({
            "id": self.id,
            "path": self.path,
            "url": self.url,
            "hash": self.hash,
            "name": self.name,
            "private": self.private,
        })
    }

    pub fn from_value(v: &Value) -> Result<Self> {
        Ok(Self {
            id: v["id"].as_str().context("package missing id")?.to_owned(),
            path: v["path"]
                .as_str()
                .context("package missing path")?
                .to_owned(),
            url: v["url"].as_str().context("package missing url")?.to_owned(),
            hash: v["hash"]
                .as_str()
                .context("package missing hash")?
                .to_owned(),
            name: v["name"]
                .as_str()
                .context("package missing name")?
                .to_owned(),
            private: v["private"]
                .as_bool()
                .context("package missing private")?,
        })
    }
}

pub struct Plan {
    pub format: u32,
    pub packages: Vec<Package>,
    pub has_patches: bool,
    pub patches: BTreeMap<String, Value>,
    pub target_packages: BTreeMap<String, BTreeSet<String>>,
    pub dependency_files: BTreeSet<String>,
}

impl Plan {
    pub fn to_value(&self) -> Value {
        let mut obj = Map::new();
        obj.insert("format".into(), json!(self.format));
        obj.insert(
            "packages".into(),
            Value::Array(self.packages.iter().map(Package::to_value).collect()),
        );
        obj.insert("hasPatches".into(), json!(self.has_patches));
        obj.insert(
            "patches".into(),
            Value::Object(
                self.patches
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            ),
        );
        obj.insert(
            "targetPackages".into(),
            Value::Object(
                self.target_packages
                    .iter()
                    .map(|(k, v)| {
                        (
                            k.clone(),
                            Value::Array(v.iter().map(|s| Value::String(s.clone())).collect()),
                        )
                    })
                    .collect(),
            ),
        );
        obj.insert(
            "dependencyFiles".into(),
            Value::Array(
                self.dependency_files
                    .iter()
                    .map(|s| Value::String(s.clone()))
                    .collect(),
            ),
        );
        Value::Object(obj)
    }

    pub fn from_value(v: &Value) -> Result<Self> {
        let format = v["format"].as_u64().context("plan missing format")? as u32;
        let packages = v["packages"]
            .as_array()
            .context("plan missing packages")?
            .iter()
            .map(Package::from_value)
            .collect::<Result<Vec<_>>>()?;
        let has_patches = v.get("hasPatches").and_then(Value::as_bool).unwrap_or(false);
        let patches = v
            .get("patches")
            .and_then(Value::as_object)
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        let target_packages = v
            .get("targetPackages")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(k, arr)| {
                        let set = arr
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|s| s.as_str().map(str::to_owned))
                            .collect();
                        (k.clone(), set)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let dependency_files = v
            .get("dependencyFiles")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|s| s.as_str().map(str::to_owned))
            .collect();
        Ok(Self {
            format,
            packages,
            has_patches,
            patches,
            target_packages,
            dependency_files,
        })
    }
}

fn is_pkg_name_segment(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'))
}

fn split_package_identity(id: &str) -> Option<(&str, &str)> {
    let rest = id.get(1..)?;
    let at = rest.find('@')? + 1;
    let name = &id[..at];
    let version = &id[at + 1..];
    if version.is_empty() {
        return None;
    }
    let valid_name = if let Some(scoped) = name.strip_prefix('@') {
        scoped
            .split_once('/')
            .is_some_and(|(scope, pkg)| is_pkg_name_segment(scope) && is_pkg_name_segment(pkg))
    } else {
        is_pkg_name_segment(name)
    };
    valid_name.then_some((name, version))
}

#[cfg(test)]
pub fn parse(input: &str) -> Result<Plan> {
    from_lock(&serde_yaml_ng::from_str(input)?, &BTreeMap::new())
}

pub fn from_lock(lock: &Value, npmrc: &BTreeMap<String, String>) -> Result<Plan> {
    ensure!(
        lock["lockfileVersion"].as_str() == Some("9.0")
            || lock["lockfileVersion"].as_f64() == Some(9.0),
        "expected a projected pnpm format 9.0 lockfile"
    );
    let importers = lock["importers"].as_object().context("missing importers")?;
    ensure!(
        importers.contains_key("."),
        "lockfile must include a root importer"
    );
    let empty = serde_json::Map::new();
    let section = |key: &str| -> Result<_> {
        match lock.get(key) {
            Some(value) => value.as_object().with_context(|| format!("invalid {key}")),
            None => Ok(&empty),
        }
    };
    let patched_dependencies = section("patchedDependencies")?;
    let mut packages = Vec::new();
    for (id, package) in section("packages")? {
        let (name, version) = split_package_identity(id)
            .with_context(|| format!("unsupported package identity: {id}"))?;
        let resolution = package["resolution"]
            .as_object()
            .context("missing resolution")?;
        ensure!(
            resolution
                .keys()
                .all(|key| ["integrity", "tarball"].contains(&key.as_str()))
                && resolution.contains_key("integrity"),
            "{id}: expected integrity and optional tarball URL"
        );
        let hash = resolution["integrity"]
            .as_str()
            .context("integrity must be a string")?;
        let (algorithm, digest) = hash.split_once('-').context("expected SRI hash")?;
        let size = match algorithm {
            "sha256" => 32,
            "sha384" => 48,
            "sha512" => 64,
            _ => bail!("{id}: expected SHA-256/384/512 SRI hash"),
        };
        ensure!(
            base64_decode(digest)?.len() == size,
            "{id}: invalid SRI digest length"
        );
        let registry = crate::config::registry(name, npmrc)?;
        let address = if let Some(tarball) = resolution.get("tarball").and_then(Value::as_str) {
            crate::config::url(&registry)?.join(tarball)?.to_string()
        } else {
            ensure!(
                !version.contains('/'),
                "URL dependencies require explicit tarball resolutions"
            );
            format!(
                "{registry}{name}/-/{}-{version}.tgz",
                name.rsplit('/').next().unwrap()
            )
        };
        let address = crate::config::url(&address)?;
        let digest = hex_encode(&Sha256::digest(id.as_bytes()));
        let filename = address
            .path_segments()
            .and_then(|mut s| s.next_back())
            .filter(|s| !s.is_empty())
            .unwrap_or("package.tgz");
        let filename: String = filename
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-') {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        packages.push(Package {
            path: format!("/drvjs/{digest}"),
            name: format!("drvjs-{filename}"),
            private: address.host_str() != Some("registry.npmjs.org"),
            id: id.clone(),
            url: address.to_string(),
            hash: hash.to_owned(),
        });
    }
    packages.sort_unstable_by(|a, b| a.id.cmp(&b.id));
    for importer in importers.values() {
        for section in ["dependencies", "devDependencies", "optionalDependencies"] {
            if let Some(deps) = importer[section].as_object() {
                for (name, dep) in deps {
                    let version = dep["version"]
                        .as_str()
                        .context("invalid dependency version")?;
                    ensure!(
                        !["file:", "workspace:"]
                            .iter()
                            .any(|prefix| version.starts_with(prefix)),
                        "{name}: local dependencies are not yet supported"
                    );
                }
            }
        }
    }
    let mut patches = BTreeMap::new();
    for id in section("snapshots")?.keys() {
        let base = crate::native::base_id(id);
        if let Some(hash) = crate::native::patch_hash(id) {
            let patch = patched_dependencies
                .values()
                .find(|p| p["hash"].as_str() == Some(hash))
                .context("missing patch declaration")?;
            patches.insert(
                format!("{base}(patch_hash={hash})"),
                serde_json::json!({
                    "base":base, "hash":hash, "path":patch["path"]
                }),
            );
        }
    }
    Ok(Plan {
        format: 1,
        packages,
        has_patches: !patched_dependencies.is_empty(),
        patches,
        target_packages: BTreeMap::new(),
        dependency_files: BTreeSet::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Value {
        serde_json::json!({"lockfileVersion": "9.0", "importers": {".": {}}, "packages": {"@scope/example@1.2.3": {"resolution": {"integrity": format!("sha512-{}", crate::hash::base64_encode(&[0;64]))}}}})
    }
    #[test]
    fn scoped_url() {
        let p = parse(&fixture().to_string()).unwrap();
        assert_eq!(
            p.packages[0].url,
            "https://registry.npmjs.org/@scope/example/-/example-1.2.3.tgz"
        );
    }
    #[test]
    fn unsupported_inputs() {
        for (key, value) in [
            ("lockfileVersion", serde_json::json!("6.0")),
            ("importers", serde_json::json!({"other": {}})),
        ] {
            let mut lock = fixture();
            lock[key] = value;
            assert!(parse(&lock.to_string()).is_err());
        }
    }
    #[test]
    fn bad_resolution() {
        for resolution in [
            serde_json::json!({"integrity":"sha512-invalid"}),
            serde_json::json!({"tarball":"https://example.org/file.tgz"}),
            serde_json::json!({"type":"git"}),
        ] {
            let mut lock = fixture();
            lock["packages"]["@scope/example@1.2.3"]["resolution"] = resolution;
            assert!(parse(&lock.to_string()).is_err());
        }
    }
    #[test]
    fn stable_fetch_identity() {
        let mut lock = fixture();
        let before = parse(&lock.to_string()).unwrap();
        lock["packages"]["z@2.0.0"] = lock["packages"]["@scope/example@1.2.3"].clone();
        assert_eq!(
            before.packages[0],
            parse(&lock.to_string()).unwrap().packages[0]
        );
    }

    #[test]
    fn patch_tools_follow_the_selected_lockfile() {
        let mut lock = fixture();
        let plain = parse(&lock.to_string()).unwrap();
        assert!(!plain.has_patches);
        lock["patchedDependencies"] = serde_json::json!({
            "@scope/example@1.2.3": {"path":"patches/example.patch", "hash":"locked"}
        });
        let patched = parse(&lock.to_string()).unwrap();
        assert!(patched.has_patches);
        assert_eq!(plain.packages, patched.packages);
    }
}
