//! Read pnpm's project graph into the snapshot layout used by the native installer.
use anyhow::{Context, Result, bail, ensure};
use serde::{
    Deserialize, Deserializer,
    de::{self, DeserializeOwned, MapAccess, Visitor},
};
use serde_json::{Value, json};
use std::borrow::Cow;

const YAML_DOCUMENT_SEPARATOR: &str = "\n---\n";
const YAML_DOCUMENT_START: &str = "---\n";
const ROOT_IMPORTER_CONFIG_DEPENDENCIES: &str = "\n  .:\n    configDependencies:";
const ROOT_IMPORTER_CONFIG_DEPENDENCIES_JSON: &str =
    "\n\t\t\".\": {\n\t\t\t\"configDependencies\":";

fn normalize_lockfile_content(content: &str) -> Cow<'_, str> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    if content.contains("\r\n") {
        Cow::Owned(content.replace("\r\n", "\n"))
    } else {
        Cow::Borrowed(content)
    }
}

fn env_only_document(rest: &str) -> Option<&str> {
    let document = rest
        .strip_suffix("\n---")
        .or_else(|| rest.strip_suffix('\n'))
        .unwrap_or(rest);
    (document.contains(ROOT_IMPORTER_CONFIG_DEPENDENCIES)
        || document.contains(ROOT_IMPORTER_CONFIG_DEPENDENCIES_JSON))
    .then_some(document)
}

fn extract_main_document(content: &str) -> Cow<'_, str> {
    match normalize_lockfile_content(content) {
        Cow::Borrowed(c) => {
            let Some(rest) = c.strip_prefix(YAML_DOCUMENT_START) else {
                return Cow::Borrowed(c);
            };
            match rest.find(YAML_DOCUMENT_SEPARATOR) {
                Some(idx) => Cow::Borrowed(&rest[idx + YAML_DOCUMENT_SEPARATOR.len()..]),
                None => Cow::Borrowed(""),
            }
        }
        Cow::Owned(c) => {
            let Some(rest) = c.strip_prefix(YAML_DOCUMENT_START) else {
                return Cow::Owned(c);
            };
            match rest.find(YAML_DOCUMENT_SEPARATOR) {
                Some(idx) => Cow::Owned(rest[idx + YAML_DOCUMENT_SEPARATOR.len()..].to_owned()),
                None => Cow::Borrowed(""),
            }
        }
    }
}

fn extract_env_document(content: &str) -> Option<Cow<'_, str>> {
    match normalize_lockfile_content(content) {
        Cow::Borrowed(c) => {
            let rest = c.strip_prefix(YAML_DOCUMENT_START)?;
            match rest.find(YAML_DOCUMENT_SEPARATOR) {
                Some(idx) => Some(Cow::Borrowed(&rest[..idx])),
                None => env_only_document(rest).map(Cow::Borrowed),
            }
        }
        Cow::Owned(c) => {
            let rest = c.strip_prefix(YAML_DOCUMENT_START)?;
            match rest.find(YAML_DOCUMENT_SEPARATOR) {
                Some(idx) => Some(Cow::Owned(rest[..idx].to_owned())),
                None => env_only_document(rest).map(|d| Cow::Owned(d.to_owned())),
            }
        }
    }
}

fn version(doc: &Value) -> Result<String> {
    let version = match &doc["lockfileVersion"] {
        Value::String(s) => s.clone(),
        Value::Number(n) if n.is_u64() => format!("{n}.0"),
        Value::Number(n) => n.to_string(),
        _ => bail!("missing pnpm lockfileVersion"),
    };
    match version.as_str() {
        "9.0" | "12.0" | "13.0" => Ok(version),
        _ => bail!("unsupported pnpm lockfileVersion {version} (supported: 9.0, 12.0, 13.0)"),
    }
}

fn boot_importer(doc: &Value) -> Option<&Value> {
    let projects = doc
        .get("importers")
        .or_else(|| doc.get("projects"))?
        .as_object()?;
    let root = projects.get(".")?.as_object()?;
    (projects.len() == 1
        && !root.is_empty()
        && root.keys().all(|key| {
            matches!(
                key.as_str(),
                "packageManagerDependencies" | "configDependencies"
            )
        }))
    .then(|| &projects["."])
}

pub struct ProjectLock {
    pub graph: Value,
    pub original_bytes: bool,
}

// Workspace discovery only needs these sections. Manual Deserialize skips
// packages and snapshots entirely without pulling in serde_derive/syn.
struct WorkspaceSections {
    lockfile_version: Value,
    importers: Option<Value>,
    projects: Option<Value>,
    settings: Option<Value>,
}

impl<'de> Deserialize<'de> for WorkspaceSections {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct SectionsVisitor;
        impl<'de> Visitor<'de> for SectionsVisitor {
            type Value = WorkspaceSections;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a pnpm lockfile mapping")
            }
            fn visit_map<M: MapAccess<'de>>(
                self,
                mut map: M,
            ) -> std::result::Result<WorkspaceSections, M::Error> {
                let mut lockfile_version = None;
                let mut importers = None;
                let mut projects = None;
                let mut settings = None;
                while let Some(key) = map.next_key::<Cow<'de, str>>()? {
                    match key.as_ref() {
                        "lockfileVersion" => lockfile_version = Some(map.next_value()?),
                        "importers" => importers = Some(map.next_value()?),
                        "projects" => projects = Some(map.next_value()?),
                        "settings" => settings = Some(map.next_value()?),
                        _ => {
                            let _ = map.next_value::<de::IgnoredAny>()?;
                        }
                    }
                }
                Ok(WorkspaceSections {
                    lockfile_version: lockfile_version
                        .ok_or_else(|| de::Error::missing_field("lockfileVersion"))?,
                    importers,
                    projects,
                    settings,
                })
            }
        }
        deserializer.deserialize_map(SectionsVisitor)
    }
}

impl WorkspaceSections {
    fn into_document(fields: Option<Self>) -> Value {
        let Some(fields) = fields else {
            return Value::Null;
        };
        let mut doc = serde_json::Map::new();
        doc.insert("lockfileVersion".into(), fields.lockfile_version);
        for (key, section) in [
            ("settings", fields.settings),
            ("importers", fields.importers),
            ("projects", fields.projects),
        ] {
            if let Some(value) = section {
                doc.insert(key.into(), value);
            }
        }
        Value::Object(doc)
    }
}

// pnpm's compact syntax adds one extension to JSON: a trailing comma.
// Leave quoted text and byte offsets intact. Serde validates the result.
fn pnpm_json(input: &str) -> Vec<u8> {
    let mut bytes = input.as_bytes().to_vec();
    let mut quoted = false;
    let mut escaped = false;
    for index in 0..bytes.len() {
        let byte = bytes[index];
        if escaped {
            escaped = false;
        } else if byte == b'\\' && quoted {
            escaped = true;
        } else if byte == b'"' {
            quoted = !quoted;
        } else if !quoted && matches!(byte, b'}' | b']') {
            let prefix = bytes[..index].trim_ascii_end();
            if let Some(before) = prefix.strip_suffix(b",")
                && let Some(previous) = before.trim_ascii_end().last()
                && !matches!(previous, b'[' | b'{' | b',' | b':')
            {
                let comma = before.len();
                bytes[comma] = b' ';
            }
        }
    }
    bytes
}

fn documents<T: DeserializeOwned>(input: &str) -> Result<(T, Option<T>)> {
    // pnpm writes v13 as JSON with trailing commas, optionally after a YAML boot
    // document. Keep the YAML stream reader for other legal YAML encodings.
    let main = extract_main_document(input);
    if main.trim_start().starts_with('{')
        && let Ok(doc) = serde_json::from_slice(&pnpm_json(&main))
    {
        if let Some(boot) = extract_env_document(input) {
            return Ok((serde_yaml_ng::from_str(&boot)?, Some(doc)));
        }
        return Ok((doc, None));
    }
    let mut docs = serde_yaml_ng::Deserializer::from_str(input).map(T::deserialize);
    let first = docs.next().context("empty pnpm lockfile")??;
    let second = docs.next().transpose()?;
    ensure!(
        docs.next().is_none(),
        "expected at most two pnpm lockfile documents"
    );
    Ok((first, second))
}

fn project_document<T: DeserializeOwned>(
    input: &str,
    value: impl Fn(T) -> Value,
) -> Result<(Value, String, bool)> {
    let (first, second) = documents::<T>(input)?;
    let first = value(first);
    let second = second.map(value);
    let first_version = version(&first)?;
    let (graph, format, single) = if let Some(root) = boot_importer(&first) {
        ensure!(
            root.get("configDependencies")
                .is_none_or(|v| v.as_object().is_some_and(|m| m.is_empty())),
            "pnpm configDependencies in the boot document are unsupported"
        );
        ensure!(
            root.get("packageManagerDependencies")
                .is_none_or(Value::is_object),
            "invalid packageManagerDependencies in pnpm boot document"
        );
        match second {
            Some(doc) if !doc.is_null() => {
                let format = version(&doc)?;
                (doc, format, false)
            }
            _ => (json!({"importers": {".": {}}}), "9.0".into(), false),
        }
    } else {
        ensure!(
            second.is_none(),
            "expected a pnpm boot document before the project lockfile"
        );
        (first, first_version, true)
    };
    ensure!(
        graph["settings"]
            .get("integrityTarget")
            .is_none_or(|v| v == "resource"),
        "drvjs requires tarball resource integrity; lockfile integrityTarget 'tar' is unsupported"
    );
    Ok((graph, format, single))
}

pub fn importer_paths(input: &str) -> Result<Vec<std::path::PathBuf>> {
    let (doc, format, _) = project_document(input, WorkspaceSections::into_document)?;
    let importers = if format == "13.0" {
        doc.get("projects").or_else(|| doc.get("importers"))
    } else {
        doc.get("importers")
    };
    crate::workspace::paths_from_importers(importers.context("missing importers")?)
}

pub fn parse(input: &str) -> Result<ProjectLock> {
    let (mut graph, format, single) = project_document(input, std::convert::identity::<Value>)?;
    if format == "13.0" {
        ensure!(
            graph
                .get("projects")
                .or_else(|| graph.get("importers"))
                .is_some(),
            "format 13.0 project lockfile has no projects"
        );
        graph["lockfileVersion"] = json!("13.0");
        graph = crate::lockfile_v13::from_next(graph)
            .context("restoring pnpm format 13.0 snapshots")?;
    }
    graph["lockfileVersion"] = json!("9.0");
    Ok(ProjectLock {
        graph,
        original_bytes: single && format == "9.0",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn original_single_document_only() {
        for version in ["'9.0'", "9.0", "9"] {
            let input = format!("---\nlockfileVersion: {version}\nimporters: {{'.': {{}}}}\n");
            assert!(parse(&input).unwrap().original_bytes);
        }
        let lock = parse("lockfileVersion: 12.0\nimporters: {'.': {}}").unwrap();
        assert!(!lock.original_bytes);
        assert_eq!(lock.graph["lockfileVersion"], "9.0");
        let lock = parse("lockfileVersion: 13.0\nprojects: {'.': {}}").unwrap();
        assert!(lock.graph["importers"]["."].is_object());
    }

    #[test]
    fn package_manager_only() {
        let input = "---\nlockfileVersion: '9.0'\nimporters: {'.': {packageManagerDependencies: {pnpm: {specifier: 12.0.0, version: 12.0.0}}}}\n---\n";
        let lock = parse(input).unwrap();
        assert_eq!(lock.graph["importers"], json!({".": {}}));
        assert!(!lock.original_bytes);
    }

    #[test]
    fn compact_json_preserves_strings_and_rejects_missing_values() {
        let input = r#"{"examples": ["literal,}", "escaped quote: \",]", "backslash: \\",],}"#;
        let value: Value = serde_json::from_slice(&pnpm_json(input)).unwrap();
        assert_eq!(
            value["examples"],
            json!(["literal,}", "escaped quote: \",]", "backslash: \\"])
        );
        for input in ["{,}", "[,]", "[1,,]", r#"{"key":,}"#] {
            assert!(serde_json::from_slice::<Value>(&pnpm_json(input)).is_err());
        }
    }

    #[test]
    fn discovery_and_export_share_document_and_path_rules() {
        let boot = "---\nlockfileVersion: '9.0'\nimporters: {'.': {packageManagerDependencies: {}}}\n---\n";
        let compact = r#"{"projects":{"a/b":{},".":{},"a-b":{},},"lockfileVersion":"13.0",}"#;
        let yaml = "lockfileVersion: 12.0\nimporters: {'a/b': {}, '.': {}, 'a-b': {}}\n";
        for doc in [
            compact.to_owned(),
            format!("{boot}{compact}").replace('\n', "\r\n"),
            yaml.into(),
        ] {
            let list = importer_paths(&doc).unwrap();
            assert_eq!(
                list,
                crate::workspace::importer_paths_from_lock(&parse(&doc).unwrap().graph).unwrap()
            );
            assert_eq!(
                list.iter().map(|p| p.to_str().unwrap()).collect::<Vec<_>>(),
                [".", "a-b", "a/b"]
            );
        }
        for input in [
            format!("{boot}{compact}\n---\n{{}}"),
            format!("---\nlockfileVersion: '9.0'\nimporters: {{'.': {{}}}}\n---\n{compact}"),
            format!("{boot}{}", compact.replace("13.0", "14.0")),
            format!("{boot}{}", compact.replace("a/b", "../outside")),
            r#"{"projects":{".":{}},"settings":{"integrityTarget":"tar"},"lockfileVersion":"13.0"}"#.into(),
        ] {
            assert!(importer_paths(&input).is_err(), "{input}");
        }
        assert_eq!(
            importer_paths(boot).unwrap(),
            [std::path::PathBuf::from(".")]
        );
    }

    #[test]
    fn rejects_unusable_graphs() {
        for (input, reason) in [
            (
                "lockfileVersion: '14.0'",
                "unsupported pnpm lockfileVersion 14.0",
            ),
            ("lockfileVersion: '13.0'\npackages: {}", "no projects"),
            (
                "lockfileVersion: '12.0'\nsettings: {integrityTarget: tar}",
                "integrityTarget",
            ),
            (
                "lockfileVersion: '9.0'\nimporters: {'.': {configDependencies: {plugin: {}}}}",
                "configDependencies",
            ),
            (
                "lockfileVersion: '9.0'\nimporters: {'.': {}}\n---\nlockfileVersion: '9.0'",
                "boot document",
            ),
            ("lockfileVersion: '9.0'\n---\n{}\n---\n{}", "at most two"),
        ] {
            assert!(parse(input).err().unwrap().to_string().contains(reason));
        }
    }
}
