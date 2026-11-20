#!/usr/bin/env python3
"""Sandboxed cache invalidation and runtime checks for partial CI workspaces."""
import base64
import hashlib
import http.server
import io
import json
import shutil
from pathlib import Path
import subprocess
import tarfile
import tempfile
import threading


def run(*args, **kwargs):
    return subprocess.check_output(args, text=True, **kwargs).strip()


def evaluate(export, value):
    return json.loads(run('nix-instantiate', '--eval', '--strict', '--json',
                          '--read-write-mode', '--option',
                          'allow-import-from-derivation', 'false', '-E',
                          f'let graph = import {export} {{}}; in {value}'))


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value))


with tempfile.TemporaryDirectory(prefix='drvjs-incremental-') as temporary:
    root = Path(temporary)
    project = root / 'project'
    archives = {}
    requests = []

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_GET(self):
            requests.append(self.path)
            body = archives.get(self.path)
            self.send_response(200 if body else 404)
            self.end_headers()
            if body:
                self.wfile.write(body)

    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        metadata = {}
        for name, version, code in [
            ('leaf', '1.0.0', 'module.exports = 1'),
            ('leaf', '2.0.0', 'module.exports = 2'),
            ('parent', '1.0.0', "module.exports = require('leaf')"),
            ('shared', '1.0.0', 'module.exports = 42'),
        ]:
            payload = io.BytesIO()
            package = {'name': name, 'version': version, 'main': 'index.js'}
            if name == 'parent':
                package['bin'] = {'parent-tool': 'cli.js'}
            files = {
                'package.json': json.dumps(package),
                'index.js': code,
                'nonce': root.name,
            }
            if name == 'parent':
                files.update({
                    'cli.js': "#!/usr/bin/env node\nconsole.log(require('leaf'));\n",
                    'module.mjs': "import leaf from 'leaf'; export default leaf;\n",
                    'workspace.cjs': "module.exports = require('b/value.json');\n",
                })
            with tarfile.open(fileobj=payload, mode='w:gz') as archive:
                for filename, text in files.items():
                    data = text.encode()
                    info = tarfile.TarInfo('package/' + filename)
                    info.size, info.mode = len(data), 0o644
                    archive.addfile(info, io.BytesIO(data))
            body = payload.getvalue()
            path = f'/{name}-{version}.tgz'
            archives[path] = body
            metadata[f'{name}@{version}'] = {'resolution': {
                'tarball': f'http://127.0.0.1:{server.server_port}{path}',
                'integrity': 'sha512-' + base64.b64encode(hashlib.sha512(body).digest()).decode(),
            }}
            if name == 'parent':
                metadata[f'{name}@{version}']['hasBin'] = True
        write_json(project / 'package.json', {'name': 'root', 'private': True})
        write_json(project / 'pnpm-workspace.yaml', {'packages': ['packages/*']})
        manifests = {
            'a': {'b': 'workspace:*', 'parent': '1.0.0'},
            'b': {'a': 'workspace:*', 'shared': '1.0.0'},
            'other': {'unreachable': '1.0.0'},
            'independent': {'shared': '1.0.0'},
        }
        importers = {'.': {}}
        for name, deps in manifests.items():
            write_json(project / 'packages' / name / 'package.json', {
                'name': name, 'version': '1.0.0', 'dependencies': deps,
            })
            (project / 'packages' / name / 'index.js').write_text(f'// {name}\n')
            importers[f'packages/{name}'] = {'dependencies': {
                alias: {'specifier': spec, 'version': f'link:../{alias}' if spec == 'workspace:*' else spec}
                for alias, spec in deps.items()
            }}
        b_manifest = project / 'packages/b/package.json'
        b_data = json.loads(b_manifest.read_text())
        b_data['bin'] = {'workspace-tool': './bin/cli.js'}
        write_json(b_manifest, b_data)
        workspace_bin = project / 'packages/b/bin/cli.js'
        workspace_bin.parent.mkdir()
        workspace_bin.write_text("#!/usr/bin/env node\nconsole.log(require('shared') + require('../value.json'));\n")
        write_json(project / 'packages/b/value.json', 8)
        # These URLs deliberately do not exist. Neither the incompatible optional
        # package nor its otherwise compatible child may enter the Nix inputs.
        a_manifest = project / 'packages/a/package.json'
        a_data = json.loads(a_manifest.read_text())
        a_data['optionalDependencies'] = {'darwin-only': '1.0.0'}
        write_json(a_manifest, a_data)
        importers['packages/a']['optionalDependencies'] = {
            'darwin-only': {'specifier': '1.0.0', 'version': '1.0.0'}}
        for name in ['darwin-only', 'darwin-child']:
            metadata[f'{name}@1.0.0'] = {'resolution': {
                'tarball': f'http://127.0.0.1:{server.server_port}/never-download-{name}.tgz',
                'integrity': 'sha512-' + base64.b64encode(bytes(64)).decode(),
            }}
        metadata['darwin-only@1.0.0']['os'] = ['darwin']
        lock = {'lockfileVersion': '9.0', 'settings': {
            'autoInstallPeers': True, 'excludeLinksFromLockfile': False},
            'importers': importers,
            'packages': {key: value for key, value in metadata.items() if key != 'leaf@2.0.0'},
            'snapshots': {'parent@1.0.0': {'dependencies': {'leaf': '1.0.0'}},
                          'leaf@1.0.0': {}, 'shared@1.0.0': {},
                          'darwin-only@1.0.0': {'optional': True, 'dependencies': {'darwin-child': '1.0.0'}},
                          'darwin-child@1.0.0': {'optional': True}},
        }

        def export(name, selector='a'):
            write_json(project / 'pnpm-lock.yaml', lock)
            destination = root / name
            run('drvjs', 'export', '--cache-packages', '-C', str(project), '--filter', selector, '-o', str(destination))
            return destination

        def contents(graph):
            return evaluate(graph, 'builtins.mapAttrs (_: v: v.drvPath) graph.packageContents')

        def build(graph, *extra):
            return Path(run('nix-build', str(graph), '--no-out-link', *extra))

        def runtime(output, version):
            run('node', '-e', f"""
                const assert = require('node:assert/strict');
                const fs = require('node:fs');
                const {{createRequire}} = require('node:module');
                const a = createRequire('{output}/packages/a/package.json');
                const b = createRequire('{output}/packages/b/package.json');
                assert.equal(a('parent'), {version});
                assert.equal(b('shared'), 42);
                assert.equal(fs.realpathSync('{output}/packages/a/node_modules/b'), '{output}/packages/b');
                assert.equal(fs.realpathSync('{output}/packages/b/node_modules/a'), '{output}/packages/a');
                assert(!fs.existsSync('{output}/packages/other'));
                import(a.resolve('parent/module.mjs')).then(m => assert.equal(m.default, {version}));
            """)
            assert run(str(output / 'packages/a/node_modules/.bin/parent-tool')) == str(version)

        original = export('original')
        original_contents = contents(original)
        original_drv = evaluate(original, 'graph.drvPath')
        first = build(original)
        runtime(first, 1)
        dependency_drv = evaluate(original, 'graph.dependencies.drvPath')
        layer = build(original, '-A', 'dependencies')
        assert not (layer / 'packages/b/value.json').exists()
        assert not (layer / 'packages/b/index.js').exists()
        assert (layer / 'packages/b/bin/cli.js').exists()
        layered = build(original, '--arg', 'cacheDependencies', 'true')
        runtime(layered, 1)
        shared = build(original, '--arg', 'sharedDependencies', 'true')
        runtime(shared, 1)
        shared_packages = Path(json.loads((shared / 'node_modules/.drvjs-shared.json').read_text())['packages'])
        shared_drv = evaluate(original, '(import ' + str(original) + ' { sharedDependencies = true; }).dependencies.packages.drvPath')
        assert shared_packages in map(Path, run('nix-store', '-qR', str(shared)).splitlines())
        assert run(str(shared / 'packages/a/node_modules/.bin/workspace-tool')) == '50'
        assert (shared / 'node_modules/.pnpm/parent@1.0.0').is_symlink()
        # The graph must not retain the assembly metadata output or any partial
        # workspace. Root package paths are shared, workspace sources are local.
        assert not (shared_packages / 'packages/b/package.json').exists()
        assert not (shared_packages / 'node_modules/.pnpm/node_modules/b').exists()
        expanded = root / 'expanded'
        run('drvjs', 'materialize', str(shared), '-o', str(expanded))
        runtime(expanded, 1)
        assert not (expanded / 'node_modules/.pnpm/parent@1.0.0').is_symlink()
        assert not (expanded / 'node_modules/.drvjs-shared.json').exists()
        # The shared layout has an explicit runtime boundary: registry packages
        # cannot use undeclared workspace sources via an ancestor's hoists.
        run('node', '-e', f"""
            const a = require('node:assert/strict');
            const {{createRequire}} = require('node:module');
            const shared = createRequire('{shared}/packages/a/package.json');
            const expanded = createRequire('{expanded}/packages/a/package.json');
            a.throws(() => shared('parent/workspace.cjs'), {{code:'MODULE_NOT_FOUND'}});
            a.equal(expanded('parent/workspace.cjs'), 8);
        """)
        (expanded / 'node_modules/.pnpm/leaf@1.0.0/node_modules/leaf/index.js').write_text('module.exports=99')
        runtime(shared, 1)
        build(original, '--arg', 'sharedDependencies', 'true', '--check')
        assert run(str(layered / 'packages/a/node_modules/.bin/workspace-tool')) == '50'
        assert str(layer) not in run('nix-store', '-qR', str(layered)).splitlines()
        build(original, '--arg', 'cacheDependencies', 'true', '--check')
        assert set(requests) == {'/parent-1.0.0.tgz', '/shared-1.0.0.tgz', '/leaf-1.0.0.tgz'}, requests
        requests.clear()

        # An unrelated workspace edit must leave even final assembly unchanged.
        (project / 'packages/other/index.js').write_text('// unrelated edit\n')
        excluded = export('excluded-edit')
        assert evaluate(excluded, 'graph.drvPath') == original_drv

        # Included sources invalidate assembly but none of the package work.
        (project / 'packages/b/index.js').write_text('// included edit\n')
        write_json(project / 'packages/b/new-directory/data.json', {'source': 'new directory'})
        write_json(project / 'packages/b/value.json', 9)
        source_edit = export('source-edit')
        assert evaluate(source_edit, 'graph.drvPath') != original_drv
        assert contents(source_edit) == original_contents
        assert evaluate(source_edit, 'graph.dependencies.drvPath') == dependency_drv
        layered_edit = build(source_edit, '--arg', 'cacheDependencies', 'true')
        assert run(str(layered_edit / 'packages/a/node_modules/.bin/workspace-tool')) == '51'
        assert (layered_edit / 'packages/b/new-directory/data.json').is_file()
        shared_edit = build(source_edit, '--arg', 'sharedDependencies', 'true')
        assert evaluate(source_edit, '(import ' + str(source_edit) + ' { sharedDependencies = true; }).dependencies.packages.drvPath') == shared_drv
        assert json.loads((shared_edit / 'node_modules/.drvjs-shared.json').read_text())['packages'] == str(shared_packages)
        assert run(str(shared_edit / 'packages/a/node_modules/.bin/workspace-tool')) == '51'
        assert (shared_edit / 'packages/b/new-directory/data.json').is_file()
        assert (build(source_edit) / 'packages/b/index.js').read_text() == '// included edit\n'
        # CI can materialize the current sources directly from the installed
        # layer. A full installation from an earlier revision is usable too.
        shared_layer = build(source_edit, '-A', 'dependencies', '--arg', 'sharedDependencies', 'true')
        for name, cached in [('standalone', layer), ('shared', shared_layer), ('previous', shared)]:
            work = root / ('work-' + name)
            run('drvjs', 'materialize', str(cached), '--project', str(source_edit / 'project'), '-o', str(work))
            runtime(work, 1)
            assert run(str(work / 'packages/a/node_modules/.bin/workspace-tool')) == '51'
            assert (work / 'packages/b/new-directory/data.json').is_file()
            assert not (work / 'node_modules/.drvjs-shared.json').exists()
            assert not (work / 'node_modules/.pnpm/parent@1.0.0').is_symlink()
            leaf = Path('node_modules/.pnpm/leaf@1.0.0/node_modules/leaf/index.js')
            original_file = (cached / leaf).read_bytes()
            (work / leaf).write_text('module.exports=99')
            assert (cached / leaf).read_bytes() == original_file
        assert not requests

        # Same package content is reusable across separate workspace selections.
        independent = export('independent', 'independent')
        assert contents(independent) == {'shared@1.0.0': original_contents['shared@1.0.0']}
        build(independent)
        assert not requests

        # Updating a transitive dependency must not re-extract its parent.
        del lock['packages']['leaf@1.0.0']
        del lock['snapshots']['leaf@1.0.0']
        lock['packages']['leaf@2.0.0'] = metadata['leaf@2.0.0']
        lock['snapshots']['leaf@2.0.0'] = {}
        lock['snapshots']['parent@1.0.0']['dependencies']['leaf'] = '2.0.0'
        updated = export('dependency-edit')
        updated_contents = contents(updated)
        assert evaluate(updated, 'graph.dependencies.drvPath') != dependency_drv
        assert updated_contents.keys() == {'parent@1.0.0', 'shared@1.0.0', 'leaf@2.0.0'}
        for name in ['parent@1.0.0', 'shared@1.0.0']:
            assert updated_contents[name] == original_contents[name]
        second = build(updated)
        runtime(second, 2)
        assert requests == ['/leaf-2.0.0.tgz'], requests
        build(updated, '--check')

        # Cached and direct assembly must agree with an independent pnpm install.
        retained = build(updated, '--arg', 'keepVerificationStore', 'true')
        run('drvjs', 'verify', str(second), '--store', str(retained / '.pnpm-store'))
        direct = build(updated, '--arg', 'cachePackages', 'false')
        run('drvjs', 'verify', str(direct), '--store', str(retained / '.pnpm-store'))
        shared_updated = build(updated, '--arg', 'sharedDependencies', 'true')
        runtime(shared_updated, 2)
        run('drvjs', 'verify', str(shared_updated), '--store', str(retained / '.pnpm-store'))
        shared_retained = build(updated, '--arg', 'sharedDependencies', 'true', '--arg', 'keepVerificationStore', 'true')
        run('drvjs', 'verify', str(shared_retained))
        # Verification must inspect the external payload, not just the small
        # project link tree. Construct a writable graph and corrupt one file.
        writable_layer, writable_graph = root / 'writable-layer', root / 'writable-graph'
        run('drvjs', 'assemble', str(updated / 'project'), str(writable_layer), '/dev/null',
            '--unpacked-store', str(retained / '.pnpm-store'), '--shared-packages', str(writable_graph))
        writable_output = root / 'writable-output'
        run('drvjs', 'attach', str(updated / 'project'), str(writable_layer), str(writable_output))
        run('drvjs', 'verify', str(writable_output), '--store', str(retained / '.pnpm-store'))
        (writable_graph / 'node_modules/.pnpm/leaf@2.0.0/node_modules/leaf/index.js').write_text('module.exports=999')
        rejected = subprocess.run(['drvjs', 'verify', str(writable_output), '--store', str(retained / '.pnpm-store')], text=True, capture_output=True)
        assert rejected.returncode and 'node_modules differs' in rejected.stderr, rejected.stderr
        layered_updated = build(updated, '--arg', 'cacheDependencies', 'true')
        runtime(layered_updated, 2)
        run('drvjs', 'verify', str(layered_updated), '--store', str(retained / '.pnpm-store'))
        layered_retained = build(updated, '--arg', 'cacheDependencies', 'true', '--arg', 'keepVerificationStore', 'true')
        run('drvjs', 'verify', str(layered_retained))
        stale = subprocess.run(['drvjs', 'attach', str(updated / 'project'), str(layer), str(root / 'stale-layer')], text=True, capture_output=True)
        assert stale.returncode and 'dependency layer lockfile differs' in stale.stderr, stale.stderr
        assert not (root / 'stale-layer').exists()

        # Workspace bin headers are assembly inputs, even when ordinary sources
        # are excluded. Changing one must invalidate the reusable layer.
        workspace_bin.write_text(workspace_bin.read_text().replace('env node', 'env -S node --no-warnings'))
        bin_edit = export('bin-edit')
        assert evaluate(bin_edit, 'graph.dependencies.drvPath') != evaluate(updated, 'graph.dependencies.drvPath')
        bin_output = build(bin_edit, '--arg', 'cacheDependencies', 'true')
        assert run(str(bin_output / 'packages/a/node_modules/.bin/workspace-tool')) == '51'

        # Cached metadata cannot silently satisfy a different locked identity.
        manifest = Path(run('nix-build', str(updated), '-A', 'contentsManifest', '--no-out-link'))
        invalid = json.loads(manifest.read_text())
        cache_paths = set(invalid.values())
        assert cache_paths <= set(run('nix-store', '-qR', str(manifest)).splitlines())
        assert not cache_paths.intersection(run('nix-store', '-qR', str(second)).splitlines())
        invalid['leaf@2.0.0'] = invalid['shared@1.0.0']
        write_json(root / 'invalid.json', invalid)
        result = subprocess.run(['drvjs', 'assemble', str(updated / 'project'),
                                 str(root / 'invalid'), '/dev/null', '--package-contents',
                                 str(root / 'invalid.json')], text=True, capture_output=True)
        assert result.returncode and 'package contents identity mismatch' in result.stderr, result.stderr

        # Patch changes rebuild only that variant; unpatched extraction stays cached.
        configured = root / 'configured'
        shutil.copytree(Path(__file__).parent / 'configured', configured)
        patched_export = root / 'patched-export'
        run('drvjs', 'export', '--cache-packages', '-C', str(configured), '-o', str(patched_export))
        patched_before = contents(patched_export)
        patched_result = build(patched_export)
        run('node', '-e', "require('node:assert/strict').equal(require('is-number/package.json').drvjsPatched, true)", cwd=patched_result)
        patch_file = configured / 'patches/is-number.patch'
        patch_text = patch_file.read_text()
        old_hash = hashlib.sha256(patch_text.encode()).hexdigest()
        patch_text = patch_text.replace('"drvjsPatched": true', '"drvjsPatched": "updated"')
        patch_file.write_text(patch_text)
        new_hash = hashlib.sha256(patch_text.encode()).hexdigest()
        lock_file = configured / 'pnpm-lock.yaml'
        lock_file.write_text(lock_file.read_text().replace(old_hash, new_hash))
        patched_update = root / 'patched-update'
        run('drvjs', 'export', '--cache-packages', '-C', str(configured), '-o', str(patched_update))
        patched_after = contents(patched_update)
        assert {key: value for key, value in patched_before.items() if '(patch_hash=' not in key} == {
            key: value for key, value in patched_after.items() if '(patch_hash=' not in key}
        assert set(patched_before.values()) - set(patched_after.values()) == {
            patched_before[f'is-number@7.0.0(patch_hash={old_hash})']}
        patched_output = build(patched_update)
        run('node', '-e', "require('node:assert/strict').equal(require('is-number/package.json').drvjsPatched, 'updated')", cwd=patched_output)
        build(patched_update, '--check')
        verification = build(patched_update, '--arg', 'keepVerificationStore', 'true')
        run('drvjs', 'verify', str(patched_output), '--store', str(verification / '.pnpm-store'))
        patched_layered = build(patched_update, '--arg', 'cacheDependencies', 'true')
        run('drvjs', 'verify', str(patched_layered), '--store', str(verification / '.pnpm-store'))
        build(patched_update, '--arg', 'cacheDependencies', 'true', '--check')
        patched_shared = build(patched_update, '--arg', 'sharedDependencies', 'true')
        run('drvjs', 'verify', str(patched_shared), '--store', str(verification / '.pnpm-store'))

        # A patch edit without a lock update still fails, even with cached contents.
        patch_file.write_text(patch_text + '\n')
        invalid_patch = root / 'invalid-patch'
        run('drvjs', 'export', '--cache-packages', '-C', str(configured), '-o', str(invalid_patch))
        result = subprocess.run(['nix-build', str(invalid_patch), '--no-out-link'], text=True, capture_output=True)
        assert result.returncode and 'patch checksum differs from lockfile' in result.stderr, result.stderr
    finally:
        server.shutdown()
        server.server_close()
print('PASS: partial workspace cycles, cross-selection reuse, source isolation, transitive dependency and patch updates, runtime, pnpm parity, reproducibility and cache identity checks')
