//! Native pnpm lockfile format 13.0 -> 9.0 snapshot restorer.
//! Replaces the 220-crate `pnpm-lockfile` git dependency with a focused,
//! zero-extra-dependency reader implementing pnpm 12.21.0's compact format.
use crate::{
    hash::Sha256,
    semver::{Range, Version},
};
use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

const DEFAULT_PEERS_SUFFIX_MAX_LENGTH: usize = 1000;

type NamedBindings = BTreeMap<String, NextRef>;

#[derive(Clone, Debug, PartialEq, Eq)]
struct NextRef {
    version: String,
    peers: Option<NamedBindings>,
}

impl NextRef {
    fn from_value(v: &Value) -> Result<Self> {
        if let Some(s) = v.as_str() {
            Ok(Self {
                version: s.to_owned(),
                peers: None,
            })
        } else if let Some(obj) = v.as_object() {
            let version = obj
                .get("version")
                .and_then(Value::as_str)
                .context("next reference missing version")?
                .to_owned();
            let peers = obj.get("with").map(parse_named_bindings).transpose()?;
            Ok(Self { version, peers })
        } else {
            bail!("invalid v13 dependency reference: {v}")
        }
    }

    fn as_link_target(&self) -> Option<&str> {
        self.version.strip_prefix("link:")
    }

    /// Resolve this reference (under `alias`) to its base `name@version` package key (without peer suffix).
    fn resolve_base(&self, alias: &str) -> Option<String> {
        if self.version.starts_with("link:") {
            return None;
        }
        let base = strip_peer_suffix(&self.version);
        if is_full_pkg_key(base) {
            Some(base.to_owned())
        } else {
            Some(format!("{alias}@{base}"))
        }
    }
}

fn parse_named_bindings(v: &Value) -> Result<NamedBindings> {
    let obj = v.as_object().context("expected object for `with` bindings")?;
    let mut map = BTreeMap::new();
    for (k, val) in obj {
        map.insert(k.clone(), NextRef::from_value(val)?);
    }
    Ok(map)
}

fn is_full_pkg_key(s: &str) -> bool {
    if s.starts_with('@') {
        return true;
    }
    if let Some(at) = s.find('@') {
        let colon = s.find(':');
        let paren = s.find('(');
        return colon.is_none_or(|c| at < c) && paren.is_none_or(|p| at < p);
    }
    false
}

fn strip_peer_suffix(s: &str) -> &str {
    s.split('(').next().unwrap_or(s)
}

pub fn split_pkg_key(key: &str) -> Option<(&str, &str)> {
    let rest = key.get(1..)?;
    let at = rest.find('@')? + 1;
    Some((&key[..at], &key[at + 1..]))
}

#[derive(Clone, Debug, Default)]
struct NextSnapshotEntry {
    id: Option<String>,
    dependencies: BTreeMap<String, NextRef>,
    optional_dependencies: BTreeMap<String, NextRef>,
    peer_dependencies: BTreeMap<String, NextRef>,
    transitive_peer_dependencies: BTreeMap<String, NextRef>,
    unbound_peer_dependencies: Option<Vec<String>>,
    optional: bool,
}

impl NextSnapshotEntry {
    fn bindings(&self) -> impl Iterator<Item = (&str, &NextRef)> {
        self.peer_dependencies
            .iter()
            .chain(self.transitive_peer_dependencies.iter())
            .map(|(k, v)| (k.as_str(), v))
    }

    fn reference_in_scope(&self, name: &str) -> Option<&NextRef> {
        self.dependencies
            .get(name)
            .or_else(|| self.optional_dependencies.get(name))
            .or_else(|| self.peer_dependencies.get(name))
            .or_else(|| self.transitive_peer_dependencies.get(name))
    }

    fn over(mut self, shared: &NextSnapshotEntry) -> Self {
        for (dst, src) in [
            (&mut self.dependencies, &shared.dependencies),
            (&mut self.optional_dependencies, &shared.optional_dependencies),
            (&mut self.peer_dependencies, &shared.peer_dependencies),
            (
                &mut self.transitive_peer_dependencies,
                &shared.transitive_peer_dependencies,
            ),
        ] {
            for (k, v) in src {
                dst.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
        if self.id.is_none() {
            self.id = shared.id.clone();
        }
        if self.unbound_peer_dependencies.is_none() {
            self.unbound_peer_dependencies = shared.unbound_peer_dependencies.clone();
        }
        self.optional |= shared.optional;
        self
    }
}

#[derive(Clone, Debug)]
struct NextImporterDep {
    specifier: String,
    version: String,
    peers: Option<NamedBindings>,
}

#[derive(Clone, Debug, Default)]
struct NextImporter {
    dependencies: BTreeMap<String, NextImporterDep>,
    dev_dependencies: BTreeMap<String, NextImporterDep>,
    optional_dependencies: BTreeMap<String, NextImporterDep>,
    dependencies_meta: Option<Value>,
    publish_directory: Option<String>,
    link_directory: Option<bool>,
}

impl NextImporter {
    fn all_deps(&self) -> impl Iterator<Item = (&str, &NextImporterDep)> {
        self.dependencies
            .iter()
            .chain(self.dev_dependencies.iter())
            .chain(self.optional_dependencies.iter())
            .map(|(k, v)| (k.as_str(), v))
    }
}

fn parse_dep_map(val: Option<&Value>) -> Result<BTreeMap<String, NextRef>> {
    let mut out = BTreeMap::new();
    if let Some(obj) = val.and_then(Value::as_object) {
        for (k, v) in obj {
            out.insert(k.clone(), NextRef::from_value(v)?);
        }
    }
    Ok(out)
}

fn parse_peer_bindings_from_snapshot(
    val: Option<&Value>,
) -> Result<(BTreeMap<String, NextRef>, BTreeMap<String, NextRef>)> {
    let mut own = BTreeMap::new();
    let mut below = BTreeMap::new();
    if let Some(obj) = val.and_then(Value::as_object) {
        for (name, entry) in obj {
            if let Some(details) = entry.as_object()
                && let Some(version) = details.get("version").and_then(Value::as_str)
            {
                let peers = details.get("with").map(parse_named_bindings).transpose()?;
                let r = NextRef {
                    version: version.to_owned(),
                    peers,
                };
                if details.get("below").and_then(Value::as_bool) == Some(true) {
                    below.insert(name.clone(), r);
                } else {
                    own.insert(name.clone(), r);
                }
            }
        }
    }
    Ok((own, below))
}

fn parse_snapshot_entry(obj: &Map<String, Value>) -> Result<NextSnapshotEntry> {
    let (peer_dependencies, transitive_peer_dependencies) =
        parse_peer_bindings_from_snapshot(obj.get("peerDependencies"))?;
    let unbound = obj
        .get("unboundPeerDependencies")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        });
    Ok(NextSnapshotEntry {
        id: obj.get("id").and_then(Value::as_str).map(str::to_owned),
        dependencies: parse_dep_map(obj.get("dependencies"))?,
        optional_dependencies: parse_dep_map(obj.get("optionalDependencies"))?,
        peer_dependencies,
        transitive_peer_dependencies,
        unbound_peer_dependencies: unbound,
        optional: obj
            .get("optional")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn split_packages(
    merged: &Map<String, Value>,
) -> Result<(Map<String, Value>, BTreeMap<String, Vec<NextSnapshotEntry>>)> {
    let mut packages = Map::new();
    let mut snapshots = BTreeMap::new();
    for (key, val) in merged {
        let obj = val
            .as_object()
            .with_context(|| format!("invalid v13 package entry: {key}"))?;
        let mut peer_ranges = Map::new();
        let mut peer_meta = Map::new();
        let (own_bindings, below_bindings) =
            parse_peer_bindings_from_snapshot(obj.get("peerDependencies"))?;
        if let Some(peers) = obj.get("peerDependencies").and_then(Value::as_object) {
            for (name, entry) in peers {
                if let Some(spec) = entry.as_str() {
                    peer_ranges.insert(name.clone(), Value::String(spec.to_owned()));
                } else if let Some(details) = entry.as_object() {
                    if let Some(spec) = details.get("specifier").and_then(Value::as_str) {
                        peer_ranges.insert(name.clone(), Value::String(spec.to_owned()));
                    }
                    if let Some(opt) = details.get("optional").and_then(Value::as_bool) {
                        peer_meta.insert(name.clone(), json!({"optional": opt}));
                    }
                }
            }
        }
        let shared = NextSnapshotEntry {
            id: obj.get("id").and_then(Value::as_str).map(str::to_owned),
            dependencies: parse_dep_map(obj.get("dependencies"))?,
            optional_dependencies: parse_dep_map(obj.get("optionalDependencies"))?,
            peer_dependencies: own_bindings,
            transitive_peer_dependencies: below_bindings,
            unbound_peer_dependencies: obj
                .get("unboundPeerDependencies")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                }),
            optional: obj
                .get("optional")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        };
        let entry_list = match obj.get("snapshots").and_then(Value::as_array) {
            Some(list) if !list.is_empty() => list
                .iter()
                .map(|item| {
                    let s = parse_snapshot_entry(
                        item.as_object()
                            .with_context(|| format!("invalid snapshot entry in {key}"))?,
                    )?;
                    Ok(s.over(&shared))
                })
                .collect::<Result<Vec<_>>>()?,
            _ => vec![shared],
        };
        snapshots.insert(key.clone(), entry_list);

        // Build v9 PackageMetadata
        let mut meta = Map::new();
        let resolution = match (obj.get("resolution"), obj.get("integrity")) {
            (None, Some(integrity)) => json!({"integrity": integrity}),
            (None, None) => {
                let (_, ver) =
                    split_pkg_key(key).with_context(|| format!("missing resolution for {key}"))?;
                let dir = ver
                    .strip_prefix("file:")
                    .with_context(|| format!("missing resolution for {key}"))?;
                json!({"type": "directory", "directory": dir})
            }
            (Some(res), Some(integrity)) => {
                let mut r = res.clone();
                if let Some(rmap) = r.as_object_mut() {
                    rmap.entry("integrity".to_owned())
                        .or_insert_with(|| integrity.clone());
                }
                r
            }
            (Some(res), None) => res.clone(),
        };
        meta.insert("resolution".into(), resolution);
        for field in [
            "specifiers",
            "version",
            "engines",
            "cpu",
            "os",
            "libc",
            "hasBin",
            "prepare",
            "bundledDependencies",
        ] {
            if let Some(v) = obj.get(field) {
                meta.insert(field.into(), v.clone());
            }
        }
        if !peer_ranges.is_empty() {
            meta.insert("peerDependencies".into(), Value::Object(peer_ranges));
        }
        if !peer_meta.is_empty() {
            meta.insert("peerDependenciesMeta".into(), Value::Object(peer_meta));
        }
        packages.insert(key.clone(), Value::Object(meta));
    }
    Ok((packages, snapshots))
}

// Implied version helpers (from implied_version.rs / linked.rs / catalogs.rs)
fn alias_name(target: &str) -> &str {
    match target.get(1..).and_then(|r| r.find('@')) {
        Some(at) => &target[..=at],
        None => target,
    }
}

fn workspace_name(target: &str) -> Option<&str> {
    let at = target.get(1..)?.find('@')? + 1;
    Some(&target[..at])
}

fn catalog_implied_version(specifier: &str) -> Option<&str> {
    let Some(target) = specifier.strip_prefix("npm:") else {
        return Some(specifier);
    };
    target.get(alias_name(target).len() + 1..)
}

fn segments(id: &str) -> impl Iterator<Item = &str> {
    id.split('/').filter(|s| !s.is_empty() && *s != ".")
}

fn link_path(from_id: &str, to_id: &str) -> String {
    let from: Vec<&str> = segments(from_id).collect();
    let to: Vec<&str> = segments(to_id).collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let parts: Vec<&str> = std::iter::repeat_n("..", from.len() - common)
        .chain(to[common..].iter().copied())
        .collect();
    if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    }
}

fn resolve_catalogs(raw: &Value) -> Result<Map<String, Value>> {
    let obj = raw.as_object().context("invalid v13 catalogs")?;
    let mut out = Map::new();
    for (cat_name, entries) in obj {
        let emap = entries.as_object().context("invalid catalog entries")?;
        let mut resolved = Map::new();
        for (pkg, entry) in emap {
            let entry_val = if let Some(spec) = entry.as_str() {
                let ver = catalog_implied_version(spec).with_context(|| {
                    format!("underivable catalog version for {pkg} in {cat_name}: {spec}")
                })?;
                json!({"specifier": spec, "version": ver})
            } else {
                entry.clone()
            };
            resolved.insert(pkg.clone(), entry_val);
        }
        out.insert(cat_name.clone(), Value::Object(resolved));
    }
    Ok(out)
}

fn named_importers(raw_projects: &Map<String, Value>) -> HashMap<String, String> {
    let mut claims: HashMap<String, Option<String>> = HashMap::new();
    for (id, val) in raw_projects {
        if let Some(name) = val.get("name").and_then(Value::as_str) {
            claims
                .entry(name.to_owned())
                .and_modify(|e| {
                    if e.as_deref() != Some(id.as_str()) {
                        *e = None;
                    }
                })
                .or_insert_with(|| Some(id.clone()));
        }
    }
    let by_name: HashMap<String, String> = claims
        .into_iter()
        .filter_map(|(k, v)| Some((k, v?)))
        .collect();
    let mut seen = HashSet::new();
    let mut dup = HashSet::new();
    for id in by_name.values() {
        if !seen.insert(id.as_str()) {
            dup.insert(id.clone());
        }
    }
    by_name
        .into_iter()
        .filter(|(_, id)| !dup.contains(id))
        .collect()
}

fn injected_paths(packages: &Map<String, Value>) -> HashMap<String, String> {
    let mut claims: HashMap<String, Option<String>> = HashMap::new();
    for key in packages.keys() {
        if let Some((name, ver)) = split_pkg_key(key)
            && let Some(path) = ver.strip_prefix("file:")
        {
            claims
                .entry(name.to_owned())
                .and_modify(|e| *e = None)
                .or_insert_with(|| Some(path.to_owned()));
        }
    }
    claims
        .into_iter()
        .filter_map(|(k, v)| Some((k, v?)))
        .collect()
}

fn implied_version(
    id: &str,
    alias: &str,
    specifier: &str,
    catalogs: Option<&Map<String, Value>>,
    injected: &HashMap<String, String>,
    linked: &HashMap<String, String>,
) -> Option<String> {
    if let Some(target) = specifier.strip_prefix("workspace:") {
        let name = workspace_name(target);
        let package = name.unwrap_or(alias);
        return match (injected.get(package), linked.get(package)) {
            (Some(path), None) => Some(match name {
                Some(n) if n != alias => format!("{n}@file:{path}"),
                _ => format!("file:{path}"),
            }),
            (None, Some(target_id)) => Some(format!("link:{}", link_path(id, target_id))),
            _ => None,
        };
    }
    if let Some(cat) = specifier.strip_prefix("catalog:") {
        let cat = if cat.is_empty() { "default" } else { cat };
        let entry = catalogs?.get(cat)?.get(alias)?;
        let cspec = entry.get("specifier")?.as_str()?;
        let cver = entry.get("version")?.as_str()?;
        return Some(match cspec.strip_prefix("npm:") {
            Some(target) => format!("{}@{cver}", alias_name(target)),
            None => cver.to_owned(),
        });
    }
    Some(specifier.strip_prefix("npm:").unwrap_or(specifier).to_owned())
}

fn parse_importers(
    raw_projects: &Map<String, Value>,
    catalogs: Option<&Map<String, Value>>,
    injected: &HashMap<String, String>,
    linked: &HashMap<String, String>,
) -> Result<BTreeMap<String, NextImporter>> {
    let mut out = BTreeMap::new();
    for (id, val) in raw_projects {
        let obj = val
            .as_object()
            .with_context(|| format!("invalid v13 project entry: {id}"))?;
        let mut imp = NextImporter {
            dependencies_meta: obj.get("dependenciesMeta").cloned(),
            publish_directory: obj
                .get("publishDirectory")
                .and_then(Value::as_str)
                .map(str::to_owned),
            link_directory: obj.get("linkDirectory").and_then(Value::as_bool),
            ..Default::default()
        };
        for (field, target) in [
            ("dependencies", &mut imp.dependencies),
            ("devDependencies", &mut imp.dev_dependencies),
            ("optionalDependencies", &mut imp.optional_dependencies),
        ] {
            if let Some(deps) = obj.get(field).and_then(Value::as_object) {
                for (alias, dep_val) in deps {
                    let dep = if let Some(spec) = dep_val.as_str() {
                        let ver = implied_version(id, alias, spec, catalogs, injected, linked)
                            .with_context(|| {
                                format!("underivable version for {alias} in {id}: {spec}")
                            })?;
                        NextImporterDep {
                            specifier: spec.to_owned(),
                            version: ver,
                            peers: None,
                        }
                    } else if let Some(dobj) = dep_val.as_object() {
                        let spec = dobj
                            .get("specifier")
                            .and_then(Value::as_str)
                            .context("importer dep missing specifier")?
                            .to_owned();
                        let peers = dobj.get("with").map(parse_named_bindings).transpose()?;
                        let ver = match dobj.get("version").and_then(Value::as_str) {
                            Some(v) => v.to_owned(),
                            None => implied_version(id, alias, &spec, catalogs, injected, linked)
                                .with_context(|| {
                                    format!("underivable version for {alias} in {id}: {spec}")
                                })?,
                        };
                        NextImporterDep {
                            specifier: spec,
                            version: ver,
                            peers,
                        }
                    } else {
                        bail!("invalid importer dep {alias} in {id}");
                    };
                    target.insert(alias.clone(), dep);
                }
            }
        }
        out.insert(id.clone(), imp);
    }
    Ok(out)
}

// Tarjan SCC (iterative, matching pnpm-lockfile/src/next_format/tarjan.rs)
struct Components {
    components: Vec<Vec<usize>>,
    component_of: HashMap<usize, usize>,
}

fn tarjan_components(
    count: usize,
    neighbors: impl Fn(usize) -> Vec<usize>,
    participates: impl Fn(usize) -> bool,
) -> Components {
    let mut index_of: Vec<Option<usize>> = vec![None; count];
    let mut low = vec![0usize; count];
    let mut on_stack = vec![false; count];
    let mut stack = Vec::new();
    let mut next_index = 0usize;
    let mut components: Vec<Vec<usize>> = (0..count)
        .filter(|&n| !participates(n))
        .map(|n| vec![n])
        .collect();
    let mut component_of = HashMap::new();

    struct Frame {
        node: usize,
        edges: Vec<usize>,
        pos: usize,
    }
    let mut frames: Vec<Frame> = Vec::new();

    for root in (0..count).filter(|&n| participates(n)) {
        if index_of[root].is_some() {
            continue;
        }
        let edges: Vec<usize> = neighbors(root)
            .into_iter()
            .filter(|&x| participates(x))
            .collect();
        index_of[root] = Some(next_index);
        low[root] = next_index;
        next_index += 1;
        stack.push(root);
        on_stack[root] = true;
        frames.push(Frame {
            node: root,
            edges,
            pos: 0,
        });

        while let Some(frame) = frames.last_mut() {
            if frame.pos >= frame.edges.len() {
                let finished = frames.pop().unwrap().node;
                if let Some(parent) = frames.last() {
                    low[parent.node] = low[parent.node].min(low[finished]);
                }
                if index_of[finished] == Some(low[finished]) {
                    let cid = components.len();
                    let mut comp = Vec::new();
                    while let Some(m) = stack.pop() {
                        on_stack[m] = false;
                        component_of.insert(m, cid);
                        comp.push(m);
                        if m == finished {
                            break;
                        }
                    }
                    components.push(comp);
                }
                continue;
            }
            let next = frame.edges[frame.pos];
            frame.pos += 1;
            let node = frame.node;
            match index_of[next] {
                None => {
                    let edges: Vec<usize> = neighbors(next)
                        .into_iter()
                        .filter(|&x| participates(x))
                        .collect();
                    index_of[next] = Some(next_index);
                    low[next] = next_index;
                    next_index += 1;
                    stack.push(next);
                    on_stack[next] = true;
                    frames.push(Frame {
                        node: next,
                        edges,
                        pos: 0,
                    });
                }
                Some(idx) if on_stack[next] => {
                    low[node] = low[node].min(idx);
                }
                Some(_) => {}
            }
        }
    }

    Components {
        components,
        component_of,
    }
}

fn link_path_to_peer_version(rel_path: &str) -> String {
    let trimmed = rel_path.trim_start_matches('.');
    let mut out = String::with_capacity(rel_path.len());
    let mut last_was_plus = true;
    for ch in trimmed.chars() {
        let replace = ch.is_control()
            || matches!(ch, '"' | '*' | '+' | '/' | ':' | '<' | '>' | '?' | '\\' | '|');
        if !replace {
            out.push(ch);
            last_was_plus = false;
        } else if !last_was_plus {
            out.push('+');
            last_was_plus = true;
        }
    }
    let end = out.trim_end_matches(['+', '.']).len();
    if end > 0 {
        out.truncate(end);
        out
    } else if rel_path.is_empty() {
        String::new()
    } else {
        "+".into()
    }
}

fn create_peer_dep_graph_hash(mut segments: Vec<String>, max_length: usize) -> String {
    segments.sort();
    let body = segments.join(")(");
    if body.len() > max_length {
        let digest = Sha256::digest(body.as_bytes());
        let mut hex = crate::nixstore::hex_lower(&digest);
        hex.truncate(32);
        format!("({hex})")
    } else {
        format!("({body})")
    }
}

fn names_binding(wanted: &NextRef, binding: &NextRef) -> bool {
    wanted.version == strip_peer_suffix(&binding.version)
        && wanted.peers.as_ref().is_none_or(|w| match &binding.peers {
            Some(actual) => actual == w,
            None => w.is_empty(),
        })
}

fn same_target(alias: &str, left: &NextRef, right: &NextRef) -> bool {
    match (left.as_link_target(), right.as_link_target()) {
        (Some(l), Some(r)) => l == r,
        (None, None) => left.resolve_base(alias) == right.resolve_base(alias),
        _ => false,
    }
}

fn importer_dep_base(alias: &str, version: &str) -> Option<String> {
    if version.starts_with("link:") {
        return None;
    }
    let base = strip_peer_suffix(version);
    if is_full_pkg_key(base) {
        Some(base.to_owned())
    } else {
        Some(format!("{alias}@{base}"))
    }
}

fn importer_target_is(alias: &str, version: &str, reference: &NextRef) -> bool {
    match (version.strip_prefix("link:"), reference.as_link_target()) {
        (Some(l), Some(r)) => l == r,
        (None, None) => {
            let own = importer_dep_base(alias, version);
            own.is_some() && own == reference.resolve_base(alias)
        }
        _ => false,
    }
}

fn with_key(original: &str, restored_key: &str) -> String {
    if original.starts_with("link:") {
        return original.to_owned();
    }
    let Some((_, suffix)) = split_pkg_key(restored_key) else {
        return original.to_owned();
    };
    if is_full_pkg_key(strip_peer_suffix(original)) {
        restored_key.to_owned()
    } else {
        suffix.to_owned()
    }
}

fn patch_group_for(
    patched_deps: Option<&Map<String, Value>>,
    pkg_key: &str,
    packages: &Map<String, Value>,
) -> String {
    let Some(patched) = patched_deps else {
        return String::new();
    };
    let Some((pkg_name, raw_ver)) = split_pkg_key(pkg_key) else {
        return String::new();
    };
    let pkg_version = packages
        .get(pkg_key)
        .and_then(|m| m.get("version"))
        .and_then(Value::as_str)
        .unwrap_or(raw_ver);

    let mut exact: Option<&str> = None;
    let mut ranges: Vec<(&str, &str)> = Vec::new();
    let mut all: Option<&str> = None;

    for (pkey, pval) in patched {
        let hash = match pval {
            Value::String(s) => s.as_str(),
            Value::Object(o) => match o.get("hash").and_then(Value::as_str) {
                Some(h) => h,
                None => continue,
            },
            _ => continue,
        };
        if let Some((pname, pver)) = split_pkg_key(pkey) {
            if pname != pkg_name {
                continue;
            }
            if pver == pkg_version {
                exact = Some(hash);
            } else {
                ranges.push((pver, hash));
            }
        } else if pkey == pkg_name {
            all = Some(hash);
        }
    }

    if let Some(h) = exact {
        return format!("(patch_hash={h})");
    }
    if let Ok(parsed) = Version::parse(pkg_version) {
        let matching: Vec<&str> = ranges
            .iter()
            .filter_map(|(r, h)| Range::parse(r).ok()?.satisfies(&parsed).then_some(*h))
            .collect();
        if matching.len() == 1 {
            return format!("(patch_hash={})", matching[0]);
        }
    }
    if let Some(h) = all {
        return format!("(patch_hash={h})");
    }
    String::new()
}

type Sid = usize;

#[derive(Clone, Copy)]
enum Referrer<'a> {
    Importer(&'a str),
    Snapshot(Sid),
}

enum Provider {
    Snapshot(Sid),
    Link(String),
    Missing(String),
}

pub fn from_next(mut next: Value) -> Result<Value> {
    let obj = next
        .as_object_mut()
        .context("v13 lockfile must be an object")?;
    let raw_projects = obj
        .remove("projects")
        .or_else(|| obj.remove("importers"))
        .and_then(|v| match v {
            Value::Object(m) => Some(m),
            _ => None,
        })
        .context("format 13.0 project lockfile has no projects")?;
    let raw_packages = obj.remove("packages").and_then(|v| match v {
        Value::Object(m) => Some(m),
        _ => None,
    });
    let mut settings_obj = obj
        .remove("settings")
        .and_then(|v| match v {
            Value::Object(m) => Some(m),
            _ => None,
        })
        .unwrap_or_default();

    let catalogs = settings_obj
        .remove("catalogs")
        .map(|v| resolve_catalogs(&v))
        .transpose()?;
    let overrides = settings_obj.remove("overrides");
    let pkg_ext_checksum = settings_obj.remove("packageExtensionsChecksum");
    let pnpmfile_checksum = settings_obj.remove("pnpmfileChecksum");
    let ignored_optional = settings_obj.remove("ignoredOptionalDependencies");
    let patched_dependencies = settings_obj.remove("patchedDependencies");

    let (packages_map, next_snapshots) = match raw_packages {
        Some(ref merged) => split_packages(merged)?,
        None => (Map::new(), BTreeMap::new()),
    };

    let injected = injected_paths(&packages_map);
    let linked = named_importers(&raw_projects);
    let importers = parse_importers(&raw_projects, catalogs.as_ref(), &injected, &linked)?;

    // Build flat snapshot graph
    let mut keys: Vec<String> = Vec::new();
    let mut entries: Vec<NextSnapshotEntry> = Vec::new();
    let mut by_package: HashMap<String, (Sid, usize)> = HashMap::new();
    for (pkg_key, list) in next_snapshots {
        let start = keys.len();
        let count = list.len();
        by_package.insert(pkg_key.clone(), (start, count));
        for entry in list {
            keys.push(pkg_key.clone());
            entries.push(entry);
        }
    }

    let snapshot_with_bindings =
        |start: Sid, count: usize, peers: &NamedBindings| -> Option<Sid> {
            (start..start + count)
                .filter(|&sid| {
                    peers.iter().all(|(name, wanted)| {
                        entries[sid]
                            .bindings()
                            .any(|(bound, r)| bound == name && names_binding(wanted, r))
                    })
                })
                .min_by_key(|&sid| entries[sid].bindings().count())
        };

    let snapshot_in_scope = |start: Sid, count: usize, referrer: Referrer<'_>| -> Sid {
        (start..start + count)
            .filter_map(|sid| {
                let mut agreeing = 0usize;
                let mut absent = 0usize;
                for (name, r) in entries[sid].bindings() {
                    let status = match referrer {
                        Referrer::Snapshot(rsid) => match entries[rsid].reference_in_scope(name) {
                            Some(scoped) => {
                                if same_target(name, scoped, r) { 1 } else { -1 }
                            }
                            None => {
                                let is_self = split_pkg_key(&keys[rsid])
                                    .is_some_and(|(n, _)| n == name)
                                    && r.resolve_base(name).as_deref() == Some(keys[rsid].as_str());
                                if is_self { 1 } else { 0 }
                            }
                        },
                        Referrer::Importer(imp_id) => match importers[imp_id]
                            .all_deps()
                            .find(|(alias, _)| *alias == name)
                        {
                            Some((_, dep)) => {
                                if importer_target_is(name, &dep.version, r) { 1 } else { -1 }
                            }
                            None => 0,
                        },
                    };
                    match status {
                        1 => agreeing += 1,
                        0 => absent += 1,
                        _ => return None,
                    }
                }
                Some((sid, agreeing, absent))
            })
            .min_by_key(|&(sid, agreeing, absent)| (std::cmp::Reverse(agreeing), absent, sid))
            .map_or(start, |(sid, _, _)| sid)
    };

    let choose = |base_key: &str, peers: Option<&NamedBindings>, referrer: Referrer<'_>| -> Option<Sid> {
        let &(start, count) = by_package.get(base_key)?;
        if count == 1 {
            return Some(start);
        }
        if let Some(p) = peers
            && let Some(sid) = snapshot_with_bindings(start, count, p)
        {
            return Some(sid);
        }
        Some(snapshot_in_scope(start, count, referrer))
    };

    // Choose children for every snapshot and importer
    let children: Vec<BTreeMap<String, Sid>> = (0..keys.len())
        .map(|sid| {
            let mut map = BTreeMap::new();
            for (alias, r) in entries[sid]
                .dependencies
                .iter()
                .chain(entries[sid].optional_dependencies.iter())
            {
                if let Some(base) = r.resolve_base(alias)
                    && let Some(child) = choose(&base, r.peers.as_ref(), Referrer::Snapshot(sid))
                {
                    map.insert(alias.clone(), child);
                }
            }
            map
        })
        .collect();

    let importer_children: BTreeMap<String, BTreeMap<String, Sid>> = importers
        .iter()
        .map(|(id, imp)| {
            let mut map = BTreeMap::new();
            for (alias, dep) in imp.all_deps() {
                if let Some(base) = importer_dep_base(alias, &dep.version)
                    && let Some(child) =
                        choose(&base, dep.peers.as_ref(), Referrer::Importer(id.as_str()))
                {
                    map.insert(alias.to_owned(), child);
                }
            }
            (id.clone(), map)
        })
        .collect();

    // Scope walk: first referrer of every snapshot
    let mut parent: Vec<Option<Referrer<'_>>> = vec![None; keys.len()];
    let mut visited = vec![false; keys.len()];
    let mut queue: Vec<Sid> = Vec::new();
    let drain = |queue: &mut Vec<Sid>,
                 visited: &mut [bool],
                 parent: &mut [Option<Referrer<'_>>]| {
        while let Some(sid) = queue.pop() {
            if std::mem::replace(&mut visited[sid], true) {
                continue;
            }
            for &child in children[sid].values() {
                if !visited[child] {
                    parent[child].get_or_insert(Referrer::Snapshot(sid));
                    queue.push(child);
                }
            }
        }
    };
    for (id, child_map) in &importer_children {
        for &child in child_map.values() {
            parent[child].get_or_insert(Referrer::Importer(id.as_str()));
            queue.push(child);
        }
        drain(&mut queue, &mut visited, &mut parent);
    }
    let mut unreached: Vec<Sid> = (0..keys.len()).filter(|&s| parent[s].is_none()).collect();
    unreached.sort_by_key(|&s| (keys[s].as_str(), s - by_package[&keys[s]].0));
    for sid in unreached {
        queue.push(sid);
        drain(&mut queue, &mut visited, &mut parent);
    }

    // Match each peer binding to its provider
    let linked_above = |sid: Sid, name: &str, start: Sid, count: usize| -> Option<Sid> {
        let mut cur = parent[sid];
        let mut steps = 0usize;
        while let Some(r) = cur {
            let found = match r {
                Referrer::Snapshot(p) => children[p].get(name).copied().or_else(|| {
                    split_pkg_key(&keys[p])
                        .is_some_and(|(n, _)| n == name)
                        .then_some(p)
                }),
                Referrer::Importer(id) => importer_children[id].get(name).copied(),
            };
            if let Some(f) = found
                && (start..start + count).contains(&f)
            {
                return Some(f);
            }
            cur = match r {
                Referrer::Snapshot(p) => parent[p],
                Referrer::Importer(_) => None,
            };
            steps += 1;
            if steps > keys.len() {
                return None;
            }
        }
        None
    };

    let providers: Vec<Vec<(String, Provider)>> = (0..keys.len())
        .map(|sid| {
            entries[sid]
                .bindings()
                .map(|(name, r)| {
                    let prov = if let Some(path) = r.as_link_target() {
                        Provider::Link(path.to_owned())
                    } else if let Some(base) = r.resolve_base(name) {
                        if let Some(&(start, count)) = by_package.get(&base) {
                            if count == 1 {
                                Provider::Snapshot(start)
                            } else if let Some(peers) = r.peers.as_ref()
                                && let Some(found) = snapshot_with_bindings(start, count, peers)
                            {
                                Provider::Snapshot(found)
                            } else if let Some(found) = linked_above(sid, name, start, count) {
                                Provider::Snapshot(found)
                            } else {
                                Provider::Snapshot(snapshot_in_scope(
                                    start,
                                    count,
                                    Referrer::Snapshot(sid),
                                ))
                            }
                        } else {
                            Provider::Missing(base)
                        }
                    } else {
                        Provider::Missing(r.version.clone())
                    };
                    (name.to_owned(), prov)
                })
                .collect()
        })
        .collect();

    // Rebuild snapshot keys
    let dedupe_peers = settings_obj
        .get("dedupePeers")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let max_length = settings_obj
        .get("peersSuffixMaxLength")
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .unwrap_or(DEFAULT_PEERS_SUFFIX_MAX_LENGTH);
    let patched_map = patched_dependencies.as_ref().and_then(Value::as_object);

    let patch_groups: Vec<String> = (0..keys.len())
        .map(|sid| patch_group_for(patched_map, &keys[sid], &packages_map))
        .collect();

    let restored_keys: Vec<String> = if dedupe_peers {
        (0..keys.len())
            .map(|sid| {
                let mut key = format!("{}{}", keys[sid], patch_groups[sid]);
                if !providers[sid].is_empty() {
                    let segs: Vec<String> = providers[sid]
                        .iter()
                        .map(|(name, prov)| match prov {
                            Provider::Link(p) => format!("{name}@{}", link_path_to_peer_version(p)),
                            Provider::Missing(s) => s.clone(),
                            Provider::Snapshot(psid) => keys[*psid].clone(),
                        })
                        .collect();
                    key.push_str(&create_peer_dep_graph_hash(segs, max_length));
                }
                key
            })
            .collect()
    } else {
        let order = tarjan_components(
            keys.len(),
            |sid| {
                providers[sid]
                    .iter()
                    .filter_map(|(_, p)| match p {
                        Provider::Snapshot(psid) => Some(*psid),
                        _ => None,
                    })
                    .collect()
            },
            |sid| !providers[sid].is_empty(),
        );
        // Cyclic peer names
        let mut pname_list: Vec<String> = Vec::new();
        let mut pname_idx: HashMap<String, usize> = HashMap::new();
        let mut pname_edges: Vec<HashSet<usize>> = Vec::new();
        let mut intern = |name: String, edges: &mut Vec<HashSet<usize>>| -> usize {
            if let Some(&i) = pname_idx.get(&name) {
                i
            } else {
                let i = pname_list.len();
                pname_list.push(name.clone());
                edges.push(HashSet::new());
                pname_idx.insert(name, i);
                i
            }
        };
        for sid in 0..keys.len() {
            if providers[sid].is_empty() {
                continue;
            }
            if let Some((from_name, _)) = split_pkg_key(&keys[sid]) {
                let from = intern(from_name.to_owned(), &mut pname_edges);
                for (to_name, _) in &providers[sid] {
                    let to = intern(to_name.clone(), &mut pname_edges);
                    pname_edges[from].insert(to);
                }
            }
        }
        let name_sccs = tarjan_components(
            pname_edges.len(),
            |n| pname_edges[n].iter().copied().collect(),
            |_| true,
        );
        let cyclic_names: HashSet<String> = name_sccs
            .components
            .iter()
            .filter(|c| match c.as_slice() {
                [only] => pname_edges[*only].contains(only),
                _ => true,
            })
            .flatten()
            .map(|&n| pname_list[n].clone())
            .collect();

        // Compute levels
        let mut level_of = vec![0usize; order.components.len()];
        let mut levels: Vec<Vec<usize>> = Vec::new();
        for (cid, members) in order.components.iter().enumerate() {
            let lvl = members
                .iter()
                .flat_map(|&sid| providers[sid].iter())
                .filter_map(|(_, p)| match p {
                    Provider::Snapshot(psid) => order.component_of.get(psid),
                    _ => None,
                })
                .filter(|&&pcid| pcid != cid)
                .map(|&pcid| level_of[pcid] + 1)
                .max()
                .unwrap_or(0);
            level_of[cid] = lvl;
            if levels.len() <= lvl {
                levels.resize_with(lvl + 1, Vec::new);
            }
            levels[lvl].push(cid);
        }

        let mut built: Vec<Option<String>> = (0..keys.len())
            .map(|sid| {
                providers[sid]
                    .is_empty()
                    .then(|| format!("{}{}", keys[sid], patch_groups[sid]))
            })
            .collect();
        for level in levels {
            let updates: Vec<(Sid, String)> = level
                .iter()
                .flat_map(|&cid| order.components[cid].iter().copied())
                .filter(|&sid| !providers[sid].is_empty())
                .map(|sid| {
                    let segs: Vec<String> = providers[sid]
                        .iter()
                        .map(|(name, prov)| match prov {
                            Provider::Link(p) => format!("{name}@{}", link_path_to_peer_version(p)),
                            Provider::Missing(s) => s.clone(),
                            Provider::Snapshot(psid) => {
                                let in_cycle = order.component_of.get(&sid).is_some()
                                    && order.component_of.get(&sid)
                                        == order.component_of.get(psid);
                                if !in_cycle && !cyclic_names.contains(name) {
                                    if let Some(pk) = built[*psid].as_ref() {
                                        return pk.strip_prefix('/').unwrap_or(pk).to_owned();
                                    }
                                }
                                keys[*psid].clone()
                            }
                        })
                        .collect();
                    let mut key = format!("{}{}", keys[sid], patch_groups[sid]);
                    key.push_str(&create_peer_dep_graph_hash(segs, max_length));
                    (sid, key)
                })
                .collect();
            for (sid, k) in updates {
                built[sid] = Some(k);
            }
        }
        built.into_iter().map(Option::unwrap).collect()
    };

    // Unbound peers for transitivePeerDependencies
    let mut unbound_names: Vec<String> = Vec::new();
    let mut unbound_ids: HashMap<String, u32> = HashMap::new();
    let own_unbound: Vec<Vec<u32>> = (0..keys.len())
        .map(|sid| {
            let Some(declared) = packages_map
                .get(&keys[sid])
                .and_then(|m| m.get("peerDependencies"))
                .and_then(Value::as_object)
            else {
                return Vec::new();
            };
            let entry = &entries[sid];
            declared
                .keys()
                .filter(|name| {
                    !entry.peer_dependencies.contains_key(*name)
                        && !entry.dependencies.contains_key(*name)
                        && !entry.optional_dependencies.contains_key(*name)
                })
                .map(|name| {
                    if let Some(&id) = unbound_ids.get(name) {
                        id
                    } else {
                        let id = unbound_names.len() as u32;
                        unbound_names.push(name.clone());
                        unbound_ids.insert(name.clone(), id);
                        id
                    }
                })
                .collect()
        })
        .collect();
    let dep_sccs = tarjan_components(
        keys.len(),
        |sid| children[sid].values().copied().collect(),
        |_| true,
    );
    let mut unbound_below: Vec<HashSet<u32>> = vec![HashSet::new(); keys.len()];
    for comp in &dep_sccs.components {
        let mut set = HashSet::new();
        for &m in comp {
            set.extend(own_unbound[m].iter().copied());
            for &child in children[m].values() {
                if dep_sccs.component_of.get(&child) != dep_sccs.component_of.get(&m) {
                    set.extend(unbound_below[child].iter().copied());
                }
            }
        }
        for &m in comp {
            unbound_below[m].clone_from(&set);
        }
    }

    // Emit v9 snapshots
    let mut out_snapshots = Map::new();
    for sid in 0..keys.len() {
        let entry = &entries[sid];
        let mut deps = Map::new();
        let mut opt_deps = Map::new();
        for (alias, r) in &entry.dependencies {
            let ver = match children[sid].get(alias) {
                Some(&child) => with_key(&r.version, &restored_keys[child]),
                None => r.version.clone(),
            };
            deps.insert(alias.clone(), Value::String(ver));
        }
        for (alias, r) in &entry.optional_dependencies {
            let ver = match children[sid].get(alias) {
                Some(&child) => with_key(&r.version, &restored_keys[child]),
                None => r.version.clone(),
            };
            opt_deps.insert(alias.clone(), Value::String(ver));
        }
        let peer_meta = packages_map
            .get(&keys[sid])
            .and_then(|m| m.get("peerDependenciesMeta"))
            .and_then(Value::as_object);
        for (name, r) in &entry.peer_dependencies {
            let optional = peer_meta
                .and_then(|m| m.get(name))
                .and_then(|m| m.get("optional"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let ver = match providers[sid]
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, p)| p)
            {
                Some(Provider::Snapshot(psid)) => with_key(&r.version, &restored_keys[*psid]),
                _ => r.version.clone(),
            };
            if optional {
                opt_deps.insert(name.clone(), Value::String(ver));
            } else {
                deps.insert(name.clone(), Value::String(ver));
            }
        }

        let mut snap = Map::new();
        if let Some(id) = &entry.id {
            snap.insert("id".into(), Value::String(id.clone()));
        }
        if !deps.is_empty() {
            snap.insert("dependencies".into(), Value::Object(deps));
        }
        if !opt_deps.is_empty() {
            snap.insert("optionalDependencies".into(), Value::Object(opt_deps));
        }
        let mut tpeers: BTreeSet<String> = entry
            .transitive_peer_dependencies
            .keys()
            .cloned()
            .collect();
        match &entry.unbound_peer_dependencies {
            Some(list) => tpeers.extend(list.iter().cloned()),
            None => {
                let declared = packages_map
                    .get(&keys[sid])
                    .and_then(|m| m.get("peerDependencies"))
                    .and_then(Value::as_object);
                for &id in &unbound_below[sid] {
                    let name = &unbound_names[id as usize];
                    if !declared.is_some_and(|d| d.contains_key(name)) {
                        tpeers.insert(name.clone());
                    }
                }
            }
        }
        if !tpeers.is_empty() {
            snap.insert(
                "transitivePeerDependencies".into(),
                Value::Array(tpeers.into_iter().map(Value::String).collect()),
            );
        }
        if entry.optional {
            snap.insert("optional".into(), Value::Bool(true));
        }
        out_snapshots.insert(restored_keys[sid].clone(), Value::Object(snap));
    }

    // Emit v9 importers
    let mut out_importers = Map::new();
    for (id, imp) in &importers {
        let chosen = &importer_children[id];
        let mut iobj = Map::new();
        for (field, map) in [
            ("dependencies", &imp.dependencies),
            ("devDependencies", &imp.dev_dependencies),
            ("optionalDependencies", &imp.optional_dependencies),
        ] {
            if map.is_empty() {
                continue;
            }
            let mut fobj = Map::new();
            for (alias, dep) in map {
                let ver = match chosen.get(alias) {
                    Some(&sid) => with_key(&dep.version, &restored_keys[sid]),
                    None => dep.version.clone(),
                };
                fobj.insert(
                    alias.clone(),
                    json!({"specifier": dep.specifier, "version": ver}),
                );
            }
            iobj.insert(field.into(), Value::Object(fobj));
        }
        if let Some(m) = &imp.dependencies_meta {
            iobj.insert("dependenciesMeta".into(), m.clone());
        }
        if let Some(p) = &imp.publish_directory {
            iobj.insert("publishDirectory".into(), Value::String(p.clone()));
        }
        if let Some(l) = imp.link_directory {
            iobj.insert("linkDirectory".into(), Value::Bool(l));
        }
        out_importers.insert(id.clone(), Value::Object(iobj));
    }

    let mut result = Map::new();
    result.insert("lockfileVersion".into(), Value::String("9.0".into()));
    if !settings_obj.is_empty() {
        result.insert("settings".into(), Value::Object(settings_obj));
    }
    if let Some(c) = catalogs {
        result.insert("catalogs".into(), Value::Object(c));
    }
    if let Some(o) = overrides {
        result.insert("overrides".into(), o);
    }
    if let Some(c) = pkg_ext_checksum {
        result.insert("packageExtensionsChecksum".into(), c);
    }
    if let Some(c) = pnpmfile_checksum {
        result.insert("pnpmfileChecksum".into(), c);
    }
    if let Some(i) = ignored_optional {
        result.insert("ignoredOptionalDependencies".into(), i);
    }
    if let Some(p) = patched_dependencies {
        result.insert("patchedDependencies".into(), p);
    }
    result.insert("importers".into(), Value::Object(out_importers));
    if !packages_map.is_empty() {
        result.insert("packages".into(), Value::Object(packages_map));
    }
    if !out_snapshots.is_empty() {
        result.insert("snapshots".into(), Value::Object(out_snapshots));
    }
    if let Some(time) = obj.remove("time") {
        result.insert("time".into(), time);
    }
    Ok(Value::Object(result))
}
