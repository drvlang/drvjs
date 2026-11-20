"""Exercise the CLI and downstream flakes against the same pnpm workspace."""
import json
from pathlib import Path
import subprocess
import tempfile

repo = Path(__file__).resolve().parents[1]


def run(*args, cwd=None, check=True):
    return subprocess.run(args, cwd=cwd, check=check, text=True, stdout=subprocess.PIPE)


with tempfile.TemporaryDirectory(prefix="drvjs-commands-") as tmp:
    root = Path(tmp) / "project"
    root.mkdir()

    def write(file, contents):
        path = root / file
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents if isinstance(contents, str) else json.dumps(contents))

    def cli(*args, check=True):
        return run("drvjs", *args, cwd=root, check=check)

    def nix(command, *args):
        options = ["--no-write-lock-file", "--override-input", "drvjs", repo]
        if command == "flake":
            return run("nix", command, args[0], *options, *args[1:], cwd=root).stdout.strip()
        return run("nix", command, *options, *args, cwd=root).stdout.strip()

    def flake(helper, extra=""):
        write("flake.nix", '''
        {
          inputs.drvjs.url = "github:ryantm/drvjs";
          outputs = { drvjs, ... }: drvjs.lib.HELPER {
            src = builtins.path {
              name = "js-source"; path = ./.;
              filter = path: _: !(builtins.elem path (map toString [ ./docs ./flake.nix ./flake.lock ]));
            };
            EXTRA
          };
        }
        '''.replace("HELPER", helper).replace("EXTRA", extra))

    def compare(script, *args):
        return script.strip().splitlines()[-1] == json.dumps(list(args), separators=(",", ":"))

    entry = "console.log(JSON.stringify(process.argv.slice(2)))"
    scripts = {"start": "node start.js", "test": "node test.js", "exit": "node -e 'process.exit(7)'"}
    write("package.json", {"name": "root", "scripts": scripts})
    lock = {"lockfileVersion": "9.0", "importers": {".": {}}}
    write("pnpm-lock.yaml", lock)
    write("start.js", entry)
    write("test.js", "require('fs').writeFileSync('generated', 'ok'); " + entry)
    cli("export")
    assert not (root / ".drvjs/project/start.js").exists()
    cli("build")
    cli("build")
    cli("build", "-o", "installed")
    assert (root / "installed").resolve() == (root / "result").resolve()
    assert (root / "result/start.js").is_file()
    assert not (root / "result/.drvjs").exists()
    assert compare(cli("run", "--", "two words", "--flag").stdout, "two words", "--flag")
    assert compare(cli("test", "--", "-v").stdout, "-v")
    assert not (root / "generated").exists()
    assert cli("run", "--script", "exit", check=False).returncode == 7
    assert cli("build", "missing", check=False).returncode != 0

    # Keep CLI-generated artifacts out of the downstream Git flake.
    run("git", "init", "-q", cwd=root)
    write(".gitignore", ".drvjs/\nresult\ninstalled\n")
    flake("mkFlake")
    run("git", "add", ".", cwd=root)
    nix("build", "--no-link")
    assert compare(nix("run", ".", "--", "one arg"), "one arg")

    write("pnpm-workspace.yaml", "packages:\n  - apps/*\n  - libs/*\n")
    for name in ("api", "worker"):
        path = "apps/" + name
        write(path + "/package.json", {"name": "@demo/" + name, "scripts": {**scripts, "report": "node report.js"},
              "dependencies": {"shared": "workspace:*"}})
        write(path + "/start.js", "console.log(require('shared')); " + entry)
        write(path + "/report.js", "console.log(require('shared'))")
        write(path + "/test.js", "require('node:assert/strict').equal(require('shared'), 42); " + entry)
        lock["importers"][path] = {"dependencies": {"shared": {
            "specifier": "workspace:*", "version": "link:../../libs/shared"}}}
    write("libs/shared/package.json", {"name": "shared", "main": "index.js"})
    write("libs/shared/index.js", "module.exports = 42")
    lock["importers"]["libs/shared"] = {}
    write("pnpm-lock.yaml", lock)
    flake("mkMonorepoFlake")
    run("git", "add", ".", cwd=root)
    system = run("nix", "eval", "--raw", "--impure", "--expr", "builtins.currentSystem").stdout
    for group in ("packages", "apps", "checks"):
        assert json.loads(nix("eval", f".#{group}.{system}", "--json", "--apply", "builtins.attrNames")) == [
            "@demo/api", "@demo/worker", "shared"]
    nix("flake", "check")
    output = Path(nix("build", ".#@demo/api", "--print-out-paths", "--no-link"))
    assert not (output / "apps/worker").exists()
    assert compare(nix("run", ".#@demo/api", "--", "via nix"), "via nix")
    assert compare(cli("test", "@demo/api", "--", "via cli").stdout, "via cli")
    cli("build", "./apps/api")
    assert (root / "result").resolve() == output

    def api_drv():
        return json.loads(nix("eval", "--json", f'.#packages.{system}."@demo/api"',
                              "--apply", "p: [ p.planner.drvPath p.drvPath ]"))

    original = api_drv()
    write("apps/worker/start.js", "// sibling edit")
    write("docs/index.md", "irrelevant")
    run("git", "add", ".", cwd=root)
    assert api_drv() == original
    write("libs/shared/index.js", "module.exports = 43")
    assert all(a != b for a, b in zip(original, api_drv()))
    flake("mkMonorepoFlake", 'commands = { server = "./apps/api"; }; script = "report"; cacheDependencies = true;')
    assert nix("run", ".#server") == "43"
print("PASS: direct builds, scripts, downstream flakes, workspace closure and source invalidation")
