"""Compare the same workspace locked by pnpm 11.28.12 and pnpm 12.21.0."""
import json
from pathlib import Path
import shutil
import subprocess
import tempfile

repo = Path(__file__).resolve().parents[1]
fixtures = repo / "tests/lockfiles"


def run(*args, cwd=None):
    return subprocess.check_output(args, cwd=cwd, text=True).strip()


with tempfile.TemporaryDirectory(prefix="drvjs-lockfiles-") as tmp:
    root = Path(tmp) / "workspace"
    consumer = root / "packages/consumer"
    consumer.mkdir(parents=True)
    manifest = {
        "name": "example", "private": True,
        "dependencies": {"consumer": "workspace:*", "react": "18.3.1", "react-dom": "17.0.2"},
    }
    (consumer / "package.json").write_text(json.dumps({
        "name": "consumer", "version": "1.0.0",
        "scripts": {"test": "node -p \"require('react/package.json').version\""},
        "dependencies": {"alias": "npm:is-number@7.0.0", "is-odd": "3.0.1",
                         "react": "17.0.2", "react-dom": "17.0.2"},
    }))
    reference = {}
    for label in ("11", "12.0", "13.0"):
        legacy = label != "13.0"
        text = (fixtures / ("pnpm-11.yaml" if legacy else "pnpm-12.yaml")).read_text()
        if label == "12.0":
            # pnpm accepts the v9-shaped 12.0 header. Its default writer emits 9.0.
            text = text.replace("lockfileVersion: '9.0'", "lockfileVersion: '12.0'", 1)
        (root / "pnpm-lock.yaml").write_text(text)
        (root / "package.json").write_text(json.dumps(dict(
            manifest, packageManager="pnpm@" + ("11.28.12" if legacy else "12.21.0"))))
        (root / "pnpm-workspace.yaml").write_text(
            "packages: [packages/*]\n" + ("" if legacy else "lockfile:\n  format: v13\n"))
        assert json.loads(run("drvjs", "workspace-info", root, "--list")) == [".", "packages/consumer"]
        for target in ("all", "consumer"):
            export = Path(tmp) / f"{label}-{target}"
            selected = [] if target == "all" else [target]
            run("drvjs", "export", "--keep-verification-store", "-C", root, "-o", export, *selected)
            plan = json.loads((export / "plan.json").read_text())
            graph = (export / "project/pnpm-lock.yaml").read_bytes()
            assert len(plan["packages"]) == (10 if target == "all" else 9)
            if label == "11":
                reference[target] = (graph, plan)
            else:
                assert (graph, plan) == reference[target]
        assert (root / "pnpm-lock.yaml").read_text() == text

    output = Path(run("nix-build", Path(tmp) / "13.0-all", "--no-out-link"))
    # Resolve React from each react-dom instance, as a real peer consumer would.
    peer = "require(require.resolve('react/package.json',{paths:[require.resolve('react-dom')]})).version"
    assert run("node", "-p", peer, cwd=output) == "18.3.1"
    assert run("node", "-p", peer, cwd=output / "packages/consumer") == "17.0.2"
    assert run("node", "-p", "require('alias')('42')", cwd=output / "packages/consumer") == "true"
    assert (output / "node_modules/consumer").resolve() == output / "packages/consumer"
    run("drvjs", "verify", output)
    assert run("drvjs", "test", "-C", root, "consumer").splitlines()[-1] == "17.0.2"

    (root / "flake.nix").write_text('''{
      inputs.drvjs.url = "github:ryantm/drvjs";
      outputs = { drvjs, ... }: drvjs.lib.mkMonorepoFlake { src = ./.; };
    }''')
    selected = Path(run("nix", "build", ".#consumer", "--no-write-lock-file", "--no-link",
                        "--print-out-paths", "--override-input", "drvjs", repo, cwd=root))
    assert run("node", "-p", peer, cwd=selected / "packages/consumer") == "17.0.2"
    assert not (selected / "node_modules/react").exists()

    # A newer lockfile records only the patch hash; its path lives in the workspace file.
    patched = Path(tmp) / "patched"
    (patched / "patches").mkdir(parents=True)
    shutil.copy(repo / "tests/configured/patches/is-number.patch", patched / "patches")
    shutil.copy(fixtures / "patched-13.yaml", patched / "pnpm-lock.yaml")
    (patched / "package.json").write_text(json.dumps({"name": "patched", "dependencies": {"is-number": "7.0.0"}}))
    (patched / "pnpm-workspace.yaml").write_text(
        "lockfile: {format: v13}\npatchedDependencies: {is-number@7.0.0: patches/is-number.patch}\n")
    run("drvjs", "build", "-C", patched)
    assert run("node", "-p", "require('is-number/package.json').drvjsPatched", cwd=patched / "result") == "true"

    # Same name and version from distinct registries must never share a plan entry.
    named = Path(tmp) / "named"
    named.mkdir()
    (named / "pnpm-workspace.yaml").write_text(
        "registries:\n  https://registry.npmjs.org/: {prefix: mirror}\n")
    (named / "package.json").write_text(json.dumps({
        "name": "named", "dependencies": {"is-number": "mirror:7.0.0", "other": "npm:is-number@7.0.0"}}))
    digest = next(p["hash"] for p in plan["packages"] if p["id"] == "is-number@7.0.0")
    plain, qualified = "is-number@7.0.0", "is-number@mirror:7.0.0"
    lock = {
        "lockfileVersion": "12.0",
        "settings": {"autoInstallPeers": True, "excludeLinksFromLockfile": False},
        "importers": {".": {"dependencies": {
            "is-number": {"specifier": "mirror:7.0.0", "version": "mirror:7.0.0"},
            "other": {"specifier": "npm:is-number@7.0.0", "version": plain}}}},
        "packages": {key: {"resolution": {"integrity": digest}} for key in (plain, qualified)},
        "snapshots": {plain: {}, qualified: {}},
    }
    (named / "pnpm-lock.yaml").write_text(json.dumps(lock))
    (named / ".npmrc").write_text("registry=https://other.example/npm/\n")
    graph = Path(tmp) / "named-graph"
    run("drvjs", "export", "-C", named, "-o", graph)
    urls = {p["id"]: p["url"] for p in json.loads((graph / "plan.json").read_text())["packages"]}
    assert urls == {
        plain: "https://other.example/npm/is-number/-/is-number-7.0.0.tgz",
        qualified: "https://registry.npmjs.org/is-number/-/is-number-7.0.0.tgz",
    }
    (named / "pnpm-workspace.yaml").write_text("packages: []\n")
    result = subprocess.run(["drvjs", "export", "-C", named, "-o", Path(tmp) / "unknown-registry"],
                            capture_output=True, text=True)
    assert result.returncode and "unknown registry prefix mirror" in result.stderr
print("PASS: pnpm 9.0, 12.0 and 13.0 inputs, shared CLI/flake selection, peer instances, patches and offline verification")
