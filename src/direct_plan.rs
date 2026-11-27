//! Direct Nix daemon planner for drvjs dynamic derivations.
//! Replaces nix-instantiate + nixpkgs evaluation with native ATerm derivation
//! registration over the recursive Nix daemon Unix socket.
use crate::{
    native_store as store,
    nixstore::{self, Derivation, DerivationOutput, TextItem},
    plan::{Package, Plan},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    os::unix::fs::symlink,
    path::Path,
};

#[derive(Default)]
struct PlanOptions {
    cache_packages: bool,
    cache_dependencies: bool,
    shared_dependencies: bool,
    keep_verification_store: bool,
    zstd_archives: bool,
    dependencies_only: bool,
}

impl PlanOptions {
    fn from_value(v: &Value) -> Self {
        let flag = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
        Self {
            cache_packages: flag("cachePackages"),
            cache_dependencies: flag("cacheDependencies"),
            shared_dependencies: flag("sharedDependencies"),
            keep_verification_store: flag("keepVerificationStore"),
            zstd_archives: flag("zstdArchives"),
            dependencies_only: flag("dependenciesOnly"),
        }
    }
}

fn fetchurl_drv(store_dir: &str, package: &Package) -> Result<(Derivation, String)> {
    let (algo, _) = package
        .hash
        .split_once('-')
        .with_context(|| format!("invalid SRI hash for {}", package.id))?;
    let digest_hex = store::sri_hex(&package.hash)?;
    let out_path =
        nixstore::make_fixed_output_path(store_dir, &package.name, false, algo, &digest_hex);
    let drv = Derivation {
        outputs: vec![DerivationOutput {
            name: "out".into(),
            path: out_path.clone(),
            hash_algo: algo.into(),
            hash: digest_hex,
        }],
        input_drvs: BTreeMap::new(),
        input_srcs: BTreeSet::new(),
        platform: "builtin".into(),
        builder: "builtin:fetchurl".into(),
        args: Vec::new(),
        env: BTreeMap::from([
            ("builder".into(), "builtin:fetchurl".into()),
            ("executable".into(), "".into()),
            (
                "impureEnvVars".into(),
                "http_proxy https_proxy ftp_proxy all_proxy no_proxy".into(),
            ),
            ("name".into(), package.name.clone()),
            ("out".into(), out_path.clone()),
            ("outputHash".into(), package.hash.clone()),
            ("outputHashAlgo".into(), "".into()),
            ("outputHashMode".into(), "flat".into()),
            ("preferLocalBuild".into(), "1".into()),
            ("system".into(), "builtin".into()),
            ("unpack".into(), "".into()),
            ("url".into(), package.url.clone()),
            ("urls".into(), package.url.clone()),
        ]),
    };
    Ok((drv, out_path))
}

fn ca_outputs(names: &[&str]) -> Vec<DerivationOutput> {
    names
        .iter()
        .map(|name| DerivationOutput {
            name: (*name).into(),
            path: String::new(),
            hash_algo: "r:sha256".into(),
            hash: String::new(),
        })
        .collect()
}

pub fn internal_symlink(target: &Path, out: &Path) -> Result<()> {
    symlink(target, out).with_context(|| {
        format!(
            "symlinking {} -> {}",
            out.display(),
            target.display()
        )
    })
}

pub fn prepare_zstd(archive: &Path, out: &Path) -> Result<()> {
    use crate::{compress, hash::{Sha512, base64_encode}};
    ensure!(!out.exists(), "output already exists");
    let raw = fs::read(archive)?;
    let tar_bytes = if raw.starts_with(&[0x1f, 0x8b]) {
        compress::gzip_decompress(&raw, raw.len() * 4)?
    } else {
        raw
    };
    let zst = compress::zstd_compress(&tar_bytes, 1)?;
    let integrity = format!("sha512-{}", base64_encode(&Sha512::digest(&zst)));
    fs::create_dir(out)?;
    fs::write(out.join("archive.tar.zst"), &zst)?;
    fs::write(out.join("integrity"), integrity)?;
    Ok(())
}

pub fn internal_plan() -> Result<()> {
    let out = env::var("out").context("internal-plan: missing out")?;
    let system = env::var("system").context("internal-plan: missing system")?;
    let drvjs_out = env::var("drvjsOut").context("internal-plan: missing drvjsOut")?;
    let graph_dir = env::var("graphDir").context("internal-plan: missing graphDir")?;
    let project_dir = env::var("projectDir").context("internal-plan: missing projectDir")?;
    let options_json = env::var("optionsJson").context("internal-plan: missing optionsJson")?;
    let git_minimal = env::var("gitMinimalOut").ok().filter(|s| !s.is_empty());
    let remote = env::var("NIX_REMOTE").context("internal-plan: missing NIX_REMOTE")?;

    let options = PlanOptions::from_value(&serde_json::from_str(&options_json)?);
    let plan = Plan::from_value(&serde_json::from_slice(
        &fs::read(Path::new(&graph_dir).join("plan.json")).context("reading plan.json")?,
    )?)?;

    let store_dir = nixstore::store_dir();
    let builder = format!("{drvjs_out}/bin/drvjs");
    let out_placeholder = nixstore::hash_placeholder("out");
    let packages_placeholder = nixstore::hash_placeholder("packages");

    let active: BTreeSet<String> = match plan.target_packages.get(&system) {
        Some(ids) => ids.clone(),
        None => plan
            .packages
            .iter()
            .map(|p| p.id.clone())
            .chain(plan.patches.keys().cloned())
            .collect(),
    };
    let active_packages: Vec<&Package> = plan
        .packages
        .iter()
        .filter(|p| active.contains(&p.id))
        .collect();

    let mut items: Vec<TextItem> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut add_drv = |drv: Derivation| -> String {
        let aterm = drv.format().into_bytes();
        let refs = drv.references();
        let path = drv.path(&store_dir, &aterm);
        if seen.insert(path.clone()) {
            items.push(TextItem {
                store_path: path.clone(),
                name: format!("{}.drv", drv.env["name"]),
                content: aterm,
                refs,
            });
        }
        path
    };

    // 1. Fixed-output builtin:fetchurl derivations for all active tarballs.
    let mut fetched_drv: BTreeMap<String, String> = BTreeMap::new();
    let mut fetched_out: BTreeMap<String, String> = BTreeMap::new();
    for pkg in &active_packages {
        let (drv, out_path) = fetchurl_drv(&store_dir, pkg)?;
        let drv_path = add_drv(drv);
        fetched_drv.insert(pkg.id.clone(), drv_path);
        fetched_out.insert(pkg.id.clone(), out_path);
    }

    let use_contents =
        options.cache_packages && !options.keep_verification_store && !options.zstd_archives;

    // 2. Optional per-package prepared contents or zstd archives.
    let mut contents_drvs: BTreeMap<String, String> = BTreeMap::new();
    let mut zstd_drvs: BTreeMap<String, String> = BTreeMap::new();

    if use_contents {
        for pkg in &active_packages {
            let mut input_drvs = BTreeMap::new();
            input_drvs.insert(
                fetched_drv[&pkg.id].clone(),
                BTreeSet::from(["out".to_owned()]),
            );
            let name = format!("{}-contents", pkg.name);
            let drv = Derivation {
                outputs: ca_outputs(&["out"]),
                input_drvs,
                input_srcs: BTreeSet::from([drvjs_out.clone()]),
                platform: system.clone(),
                builder: builder.clone(),
                args: vec![
                    "prepare-package".into(),
                    fetched_out[&pkg.id].clone(),
                    out_placeholder.clone(),
                    pkg.id.clone(),
                    pkg.hash.clone(),
                ],
                env: BTreeMap::from([
                    ("allowSubstitutes".into(), "1".into()),
                    ("builder".into(), builder.clone()),
                    ("name".into(), name),
                    ("out".into(), out_placeholder.clone()),
                    ("outputHashAlgo".into(), "sha256".into()),
                    ("outputHashMode".into(), "recursive".into()),
                    ("system".into(), system.clone()),
                ]),
            };
            let drv_path = add_drv(drv);
            contents_drvs.insert(pkg.id.clone(), drv_path);
        }
        for (id, patch) in &plan.patches {
            if !active.contains(id) {
                continue;
            }
            let base = patch["base"].as_str().context("patch missing base")?;
            let hash = patch["hash"].as_str().context("patch missing hash")?;
            let rel_path = patch["path"].as_str().context("patch missing path")?;
            let git_out = git_minimal
                .as_ref()
                .context("patchedDependencies requires gitMinimal")?;
            let base_drv = contents_drvs
                .get(base)
                .with_context(|| format!("missing base package contents for {base}"))?
                .clone();
            let mut input_drvs = BTreeMap::new();
            input_drvs.insert(base_drv.clone(), BTreeSet::from(["out".to_owned()]));
            let patch_file = format!("{graph_dir}/project/{rel_path}");
            let drv = Derivation {
                outputs: ca_outputs(&["out"]),
                input_drvs,
                input_srcs: BTreeSet::from([
                    drvjs_out.clone(),
                    graph_dir.clone(),
                    git_out.clone(),
                ]),
                platform: system.clone(),
                builder: builder.clone(),
                args: vec![
                    "patch-package".into(),
                    nixstore::downstream_placeholder(&base_drv, "out"),
                    out_placeholder.clone(),
                    patch_file,
                    hash.into(),
                ],
                env: BTreeMap::from([
                    ("PATH".into(), format!("{git_out}/bin")),
                    ("allowSubstitutes".into(), "1".into()),
                    ("builder".into(), builder.clone()),
                    ("name".into(), "drvjs-patched-package".into()),
                    ("out".into(), out_placeholder.clone()),
                    ("outputHashAlgo".into(), "sha256".into()),
                    ("outputHashMode".into(), "recursive".into()),
                    ("system".into(), system.clone()),
                ]),
            };
            let drv_path = add_drv(drv);
            contents_drvs.insert(id.clone(), drv_path);
        }
    } else if options.zstd_archives {
        for pkg in &active_packages {
            let mut input_drvs = BTreeMap::new();
            input_drvs.insert(
                fetched_drv[&pkg.id].clone(),
                BTreeSet::from(["out".to_owned()]),
            );
            let name = format!("{}-zstd", pkg.name);
            let drv = Derivation {
                outputs: ca_outputs(&["out"]),
                input_drvs,
                input_srcs: BTreeSet::from([drvjs_out.clone()]),
                platform: system.clone(),
                builder: builder.clone(),
                args: vec![
                    "prepare-zstd".into(),
                    fetched_out[&pkg.id].clone(),
                    out_placeholder.clone(),
                ],
                env: BTreeMap::from([
                    ("allowSubstitutes".into(), "1".into()),
                    ("builder".into(), builder.clone()),
                    ("name".into(), name),
                    ("out".into(), out_placeholder.clone()),
                    ("outputHashAlgo".into(), "sha256".into()),
                    ("outputHashMode".into(), "recursive".into()),
                    ("system".into(), system.clone()),
                ]),
            };
            let drv_path = add_drv(drv);
            zstd_drvs.insert(pkg.id.clone(), drv_path);
        }
    }

    // 3. Helper to construct an assemble derivation (either full or dependencies layer).
    let make_assemble = |src_root: &str, src_store_path: &str, drv_name: &str, shared: bool| -> Result<Derivation> {
        let mut input_drvs: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut input_srcs: BTreeSet<String> = BTreeSet::from([
            drvjs_out.clone(),
            src_store_path.to_owned(),
        ]);
        let mut env = BTreeMap::from([
            ("allowSubstitutes".into(), "1".into()),
            ("builder".into(), builder.clone()),
            ("name".into(), drv_name.to_owned()),
            ("out".into(), out_placeholder.clone()),
            ("outputHashAlgo".into(), "sha256".into()),
            ("outputHashMode".into(), "recursive".into()),
            ("preferLocalBuild".into(), "1".into()),
            ("system".into(), system.clone()),
        ]);
        let mut args = vec![
            "assemble".into(),
            src_root.to_owned(),
            out_placeholder.clone(),
        ];

        let needs_git = !use_contents && plan.has_patches;
        if needs_git {
            let git_out = git_minimal
                .as_ref()
                .context("patchedDependencies requires gitMinimal")?;
            input_srcs.insert(git_out.clone());
            env.insert("PATH".into(), format!("{git_out}/bin"));
        } else {
            env.insert("PATH".into(), String::new());
        }

        if use_contents {
            args.push("@empty".into());
            args.push("--package-contents".into());
            args.push("@env:packageContentsJsonPath".into());
            let mut manifest = serde_json::Map::new();
            for (id, drv_path) in &contents_drvs {
                input_drvs.insert(drv_path.clone(), BTreeSet::from(["out".to_owned()]));
                manifest.insert(
                    id.clone(),
                    Value::String(nixstore::downstream_placeholder(drv_path, "out")),
                );
            }
            env.insert(
                "packageContentsJson".into(),
                serde_json::to_string(&Value::Object(manifest))?,
            );
            env.insert("passAsFile".into(), "packageContentsJson".into());
        } else if options.zstd_archives {
            args.push("@env:tarballsJsonPath".into());
            let mut list = Vec::with_capacity(active_packages.len());
            for pkg in &active_packages {
                let drv_path = &zstd_drvs[&pkg.id];
                input_drvs.insert(drv_path.clone(), BTreeSet::from(["out".to_owned()]));
                let dir = nixstore::downstream_placeholder(drv_path, "out");
                list.push(json!({
                    "id": pkg.id,
                    "path": pkg.path,
                    "file": format!("{dir}/archive.tar.zst"),
                    "sourceIntegrity": pkg.hash,
                    "integrityFile": format!("{dir}/integrity"),
                }));
            }
            env.insert("tarballsJson".into(), serde_json::to_string(&list)?);
            env.insert("passAsFile".into(), "tarballsJson".into());
        } else {
            args.push("@env:tarballsJsonPath".into());
            let mut list = Vec::with_capacity(active_packages.len());
            for pkg in &active_packages {
                input_drvs.insert(
                    fetched_drv[&pkg.id].clone(),
                    BTreeSet::from(["out".to_owned()]),
                );
                list.push(json!({
                    "id": pkg.id,
                    "path": pkg.path,
                    "file": fetched_out[&pkg.id],
                }));
            }
            env.insert("tarballsJson".into(), serde_json::to_string(&list)?);
            env.insert("passAsFile".into(), "tarballsJson".into());
        }

        if options.keep_verification_store {
            args.push("--keep-verification-store".into());
        }
        let outputs = if shared {
            args.push("--shared-packages".into());
            args.push(packages_placeholder.clone());
            env.insert("outputs".into(), "out packages".into());
            env.insert("packages".into(), packages_placeholder.clone());
            ca_outputs(&["out", "packages"])
        } else {
            ca_outputs(&["out"])
        };

        Ok(Derivation {
            outputs,
            input_drvs,
            input_srcs,
            platform: system.clone(),
            builder: builder.clone(),
            args,
            env,
        })
    };

    // 4. Emit the top-level derivation directly to $out (no redundant inner forwarder).
    let deps_project = format!("{graph_dir}/project");
    let top_drv = if options.dependencies_only {
        make_assemble(
            &deps_project,
            &graph_dir,
            "drvjs-dependencies",
            options.shared_dependencies,
        )?
    } else if options.cache_dependencies || options.shared_dependencies {
        let deps = make_assemble(
            &deps_project,
            &graph_dir,
            "drvjs-dependencies",
            options.shared_dependencies,
        )?;
        let deps_drv_path = add_drv(deps);
        let mut input_drvs = BTreeMap::new();
        input_drvs.insert(deps_drv_path.clone(), BTreeSet::from(["out".to_owned()]));
        Derivation {
            outputs: ca_outputs(&["out"]),
            input_drvs,
            input_srcs: BTreeSet::from([drvjs_out.clone(), project_dir.clone()]),
            platform: system.clone(),
            builder: builder.clone(),
            args: vec![
                "attach".into(),
                project_dir.clone(),
                nixstore::downstream_placeholder(&deps_drv_path, "out"),
                out_placeholder.clone(),
            ],
            env: BTreeMap::from([
                ("allowSubstitutes".into(), "1".into()),
                ("builder".into(), builder.clone()),
                ("name".into(), "drvjs-node-modules".into()),
                ("out".into(), out_placeholder.clone()),
                ("outputHashAlgo".into(), "sha256".into()),
                ("outputHashMode".into(), "recursive".into()),
                ("preferLocalBuild".into(), "1".into()),
                ("system".into(), system.clone()),
            ]),
        }
    } else {
        make_assemble(&project_dir, &project_dir, "drvjs-node-modules", false)?
    };

    let top_aterm = top_drv.format();
    nixstore::register_text_graph(&remote, items)?;
    fs::write(&out, top_aterm.as_bytes()).with_context(|| format!("writing {out}"))?;
    Ok(())
}
