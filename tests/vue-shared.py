#!/usr/bin/env python3
"""Build pinned Vue compiler packages with standalone and shared installations.

Pass built full-workspace outputs for Vue v3.5.22. Requires drvjs, Node and Git.
Creates writable workspaces inside --out; neither Nix output is modified.
"""
import argparse
import hashlib
import os
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--standalone', type=Path, required=True)
parser.add_argument('--shared', type=Path, required=True)
parser.add_argument('--out', type=Path, required=True)
args = parser.parse_args()
args.out.mkdir(parents=True, exist_ok=False)
root = args.out.resolve()
outputs = {}
for mode in ['standalone', 'shared']:
    source = str(getattr(args, mode).resolve())
    workspace = root / mode
    command = ['drvjs', 'materialize', source, '-o', str(workspace)] if mode == 'standalone' else ['drvjs', 'attach', source, source, str(workspace)]
    subprocess.run(command, check=True)
    # Vue's enum scanner calls git grep. Exports intentionally omit .git, so
    # recreate only a temporary index of the exported sources for both layouts.
    subprocess.run(['git', 'init', '-q', str(workspace)], check=True)
    subprocess.run(['git', 'add', '--', '.'], cwd=workspace, check=True)
    env = dict(os.environ, PATH=str(workspace / 'node_modules/.bin') + ':' + os.environ['PATH'], CI='true')
    with (root / (mode + '-build.log')).open('w') as log:
        subprocess.run(['node', 'scripts/build.js', 'shared', 'compiler-core', 'compiler-dom', 'compiler-ssr', 'compiler-sfc', '-f', 'cjs', '-d'],
                       cwd=workspace, env=env, stdout=log, stderr=log, check=True)
    runtime = """
        const a = require('node:assert/strict');
        const sfc = require('./packages/compiler-sfc/dist/compiler-sfc.cjs.js');
        const parsed = sfc.parse('<template><div>{{ message }}</div></template><script setup>const message = "works"</script>');
        a.equal(parsed.errors.length, 0);
        a.equal(sfc.compileTemplate({source:parsed.descriptor.template.content, filename:'Example.vue', id:'example'}).errors.length, 0);
        a(sfc.compileScript(parsed.descriptor, {id:'example'}).content.includes('message'));
        const swc = require('@swc/core');
        a(swc.transformSync('const n: number = 42', {jsc:{parser:{syntax:'typescript'}}}).code.includes('42'));
        a.equal(require('rollup/parseAst').parseAst('const n = 1').type, 'Program');
    """
    subprocess.run(['node', '-e', runtime], cwd=workspace, env=env, check=True)
    outputs[mode] = {str(p.relative_to(workspace)): hashlib.sha256(p.read_bytes()).hexdigest()
                     for p in workspace.glob('packages/*/dist/*.js')}
    if mode == 'shared':
        with (root / 'shared-vitest.log').open('w') as log:
            subprocess.run(['node', 'node_modules/vitest/vitest.mjs', 'run', '--project', 'unit', '--maxWorkers', '4',
                            'packages/compiler-sfc/__tests__/parse.spec.ts', 'packages/compiler-sfc/__tests__/compileTemplate.spec.ts'],
                           cwd=workspace, env=env, stdout=log, stderr=log, check=True)
assert len(outputs['shared']) == 5, outputs
assert outputs['standalone'] == outputs['shared'], outputs
print('PASS: identical Vue compiler builds, compiled SFC runtime, SWC, Rollup native parser and shared Vitest tests')
