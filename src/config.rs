//! Configuration copied into an export must never contain authentication values.
use anyhow::{Context, Result, ensure};
use pnpm_lockfile::{PkgNameVerPeer, RegistryServerType, TarballUrlOptions, npm_tarball_url};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};
use url::Url;

pub fn url(value: &str) -> Result<Url> {
    let parsed = download_url(value)?;
    ensure!(
        parsed.query().is_none(),
        "registry/tarball URLs must not contain query strings"
    );
    Ok(parsed)
}

// Redirects may carry ephemeral signed queries. They stay host-side and are
// never written to plans, derivations, logs, or credential files.
pub fn download_url(value: &str) -> Result<Url> {
    let parsed = Url::parse(value).context("invalid registry/tarball URL")?;
    ensure!(
        parsed.username().is_empty() && parsed.password().is_none() && parsed.fragment().is_none(),
        "registry/tarball URLs must not contain credentials or fragments"
    );
    ensure!(
        parsed.scheme() == "https"
            || (parsed.scheme() == "http"
                && matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))),
        "use HTTPS for registries (HTTP is allowed only on loopback for tests)"
    );
    Ok(parsed)
}

pub fn parse_npmrc(text: &str) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .context("expected key=value in .npmrc")?;
        ensure!(
            !key.trim().ends_with("[]") || !result.contains_key(key.trim()),
            "duplicate array .npmrc setting is unsupported: {}",
            key.trim()
        );
        result.insert(
            key.trim().to_owned(),
            value.trim().trim_matches('"').trim_matches('\'').to_owned(),
        );
    }
    Ok(result)
}

pub fn npmrc(project: &Path) -> Result<BTreeMap<String, String>> {
    let path = project.join(".npmrc");
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let input = parse_npmrc(&fs::read_to_string(path)?)?;
    let mut sanitized = BTreeMap::new();
    for (key, value) in input {
        // Scope credentials are accepted as input but only the host fetch command
        // reads them. They are never included in plan.json or project snapshots.
        if key.starts_with("//")
            || [
                "_authToken",
                "_auth",
                "_password",
                "username",
                "email",
                "always-auth",
            ]
            .contains(&key.as_str())
        {
            continue;
        }
        ensure!(
            !value.contains("${"),
            "environment expansion in non-auth .npmrc settings is unsupported: {key}"
        );
        if key == "registry" || key.ends_with(":registry") {
            url(&value)?;
        } else {
            ensure!(
                [
                    "strict-peer-dependencies",
                    "engine-strict",
                    "link-workspace-packages",
                    "ignore-workspace-root-check",
                    "shared-workspace-lockfile",
                    "auto-install-peers",
                    "dedupe-peer-dependents",
                    "public-hoist-pattern[]",
                    "hoist-pattern[]",
                    "shamefully-hoist"
                ]
                .contains(&key.as_str()),
                "unsupported .npmrc setting: {key}"
            );
        }
        sanitized.insert(key, value);
    }
    Ok(sanitized)
}

pub fn registry(name: &str, settings: &BTreeMap<String, String>) -> Result<String> {
    let scoped = name
        .starts_with('@')
        .then(|| format!("{}:registry", name.split('/').next().unwrap()));
    let value = scoped
        .as_ref()
        .and_then(|scope| settings.get(scope))
        .or_else(|| settings.get("registry"))
        .map(String::as_str)
        .unwrap_or("https://registry.npmjs.org/");
    url(value)?;
    Ok(format!("{}/", value.trim_end_matches('/')))
}

// Project newer workspace registry declarations onto the explicit tarball URLs
// and scope routes the frozen pnpm 10 installer can consume.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WorkspaceRegistry {
    prefix: Option<String>,
    #[serde(default)]
    scopes: Vec<String>,
    server_type: Option<RegistryServerType>,
}

struct RegistryRoutes {
    prefixes: BTreeMap<String, String>,
    artifactory: BTreeSet<String>,
}

fn registry_routes(
    config: Option<&mut Value>,
    npmrc: &mut BTreeMap<String, String>,
) -> Result<RegistryRoutes> {
    let mut prefixes = BTreeMap::from([
        ("gh".to_owned(), "https://npm.pkg.github.com/".to_owned()),
        ("npmjs".to_owned(), "https://registry.npmjs.org/".to_owned()),
    ]);
    let mut artifactory = BTreeSet::new();
    if let Some(settings) = config.and_then(Value::as_object_mut) {
        if let Some(named) = settings.remove("namedRegistries") {
            for (prefix, address) in serde_json::from_value::<BTreeMap<String, String>>(named)? {
                prefixes.insert(prefix, url(&address)?.to_string());
            }
        }
        if let Some(registries) = settings.remove("registries") {
            for (key, entry) in registries
                .as_object()
                .context("registries must be an object")?
            {
                let (address, scopes) = if let Some(address) = entry.as_str() {
                    (url(address)?.to_string(), vec![key.clone()])
                } else {
                    let address = url(key)?.to_string();
                    let entry: WorkspaceRegistry = serde_json::from_value(entry.clone())
                        .context("invalid npm workspace registry declaration")?;
                    ensure!(
                        entry.server_type != Some(RegistryServerType::Pnpr),
                        "pnpr registry revisions and tar archives are unsupported"
                    );
                    if let Some(prefix) = entry.prefix {
                        prefixes.insert(prefix, address.clone());
                    }
                    if entry.server_type == Some(RegistryServerType::Artifactory) {
                        artifactory.insert(address.trim_end_matches('/').to_owned());
                    }
                    (address, entry.scopes)
                };
                for scope in scopes {
                    let key = if scope == "default" || scope == "@" {
                        "registry".to_owned()
                    } else {
                        ensure!(
                            scope.starts_with('@')
                                && scope[1..]
                                    .bytes()
                                    .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c)),
                            "invalid registry scope"
                        );
                        format!("{scope}:registry")
                    };
                    npmrc.insert(key, address.clone());
                }
            }
        }
    }
    Ok(RegistryRoutes {
        prefixes,
        artifactory,
    })
}

pub fn project_registries(
    lock: &mut Value,
    config: Option<&mut Value>,
    npmrc: &mut BTreeMap<String, String>,
) -> Result<bool> {
    let routes = registry_routes(config, npmrc)?;
    let Some(packages) = lock.get_mut("packages").and_then(Value::as_object_mut) else {
        return Ok(false);
    };
    let mut changed = false;
    for (id, package) in packages {
        let resolution = package
            .get_mut("resolution")
            .and_then(Value::as_object_mut)
            .with_context(|| format!("{id}: missing resolution object"))?;
        ensure!(
            resolution.get("revision").is_none(),
            "{id}: registry revisions are unsupported"
        );
        ensure!(
            resolution.get("variants").is_none(),
            "{id}: registry platform variants are unsupported"
        );
        let key: PkgNameVerPeer = id
            .parse()
            .with_context(|| format!("invalid package identity: {id}"))?;
        let name = key.name.to_string();
        let prefix_version = key.suffix.registry_qualified();
        let registry = if let Some((prefix, _)) = prefix_version {
            if resolution
                .get("tarball")
                .and_then(Value::as_str)
                .is_some_and(|t| Url::parse(t).is_ok())
            {
                continue;
            }
            routes
                .prefixes
                .get(prefix)
                .with_context(|| format!("{id}: unknown registry prefix {prefix}"))?
                .clone()
        } else {
            registry(&name, npmrc)?
        };
        let server_type = routes
            .artifactory
            .contains(registry.trim_end_matches('/'))
            .then_some(RegistryServerType::Artifactory);
        if prefix_version.is_none() && server_type.is_none() {
            continue;
        }
        let address = if let Some(tarball) = resolution.get("tarball").and_then(Value::as_str) {
            url(&format!("{}/", registry.trim_end_matches('/')))?
                .join(tarball)?
                .to_string()
        } else {
            let version = key.suffix.registry_version().with_context(|| {
                format!("{id}: expected a registry version or explicit tarball URL")
            })?;
            npm_tarball_url(
                &name,
                &version.to_string(),
                TarballUrlOptions {
                    registry: &registry,
                    server_type,
                },
            )
        };
        resolution.insert("tarball".into(), url(&address)?.to_string().into());
        changed = true;
    }
    Ok(changed)
}

pub fn validate_settings(settings: &Value) -> Result<()> {
    let settings = settings
        .as_object()
        .context("pnpm settings must be an object")?;
    if let Some(lockfile) = settings.get("lockfile") {
        for (option, value) in lockfile
            .as_object()
            .context("lockfile settings must be an object")?
        {
            ensure!(
                match option.as_str() {
                    "format" => matches!(value.as_str(), Some("v9" | "v13")),
                    "integrityTarget" => value == "resource",
                    _ => false,
                },
                "unsupported pnpm lockfile setting: {option} = {value}"
            );
        }
    }
    for key in settings.keys() {
        ensure!(
            [
                "packages",
                "lockfile",
                "namedRegistries",
                "registries",
                "catalog",
                "catalogs",
                "onlyBuiltDependencies",
                "ignoredBuiltDependencies",
                "neverBuiltDependencies",
                "allowedDeprecatedVersions",
                "peerDependencyRules",
                "overrides",
                "packageExtensions",
                "patchedDependencies",
                "supportedArchitectures",
                "publicHoistPatterns",
                "publicHoistPattern",
                "hoistPattern",
                "minimumReleaseAge",
                "minimumReleaseAgeExclude",
                "allowUnusedPatches",
                "ignorePatchFailures"
            ]
            .contains(&key.as_str()),
            "unsupported pnpm setting: {key}"
        );
    }
    Ok(())
}

pub fn safe_file(project: &Path, relative: &str) -> Result<std::path::PathBuf> {
    let path = Path::new(relative);
    ensure!(
        !path.is_absolute()
            && !path
                .components()
                .any(|p| matches!(p, std::path::Component::ParentDir)),
        "support file must be inside project: {relative}"
    );
    let resolved = project.join(path).canonicalize()?;
    ensure!(
        resolved.starts_with(project.canonicalize()?),
        "support file escapes project root: {relative}"
    );
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_deprecation_warning_and_never_build_settings() {
        for settings in [
            serde_json::json!({"allowedDeprecatedVersions":{"example":"1.0.0"}}),
            serde_json::json!({"neverBuiltDependencies":["example"]}),
            serde_json::json!({"allowedDeprecatedVersions":{"example":"1.0.0"},"neverBuiltDependencies":["example"]}),
        ] {
            validate_settings(&settings).unwrap();
        }
        assert!(validate_settings(&serde_json::json!({"unknownSetting":true})).is_err());
    }
    #[test]
    fn scopes_and_credentials() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join(".npmrc"), "@private:registry=https://example.org/npm/\n//example.org/npm/:_authToken=TOPSECRET\nengine-strict=true\n").unwrap();
        let rc = npmrc(root.path()).unwrap();
        assert!(!format!("{rc:?}").contains("TOPSECRET"));
        assert_eq!(
            registry("@private/pkg", &rc).unwrap(),
            "https://example.org/npm/"
        );
        assert_eq!(
            registry("public", &rc).unwrap(),
            "https://registry.npmjs.org/"
        );
    }
    #[test]
    fn repeated_scalar_settings_use_last_value() {
        let rc = parse_npmrc("registry=https://first.example/\nregistry=https://last.example/\n")
            .unwrap();
        assert_eq!(rc["registry"], "https://last.example/");
        assert!(parse_npmrc("hoist-pattern[]=a\nhoist-pattern[]=b\n").is_err());
    }
    #[test]
    fn rejects_embedded_credentials() {
        assert!(url("https://user:password@example.org/a").is_err());
        assert!(url("https://example.org/a?token=secret").is_err());
        assert!(url("file:///etc/passwd").is_err());
    }
}
