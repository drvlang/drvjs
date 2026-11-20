#!/usr/bin/env python3
"""Time export, writable workspace preparation and an optional CI command after source edits."""
import argparse
import json
import os
from pathlib import Path
import shutil
import statistics
import subprocess
import sys
import tempfile
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--baseline', required=True, help='drvjs binary for build followed by materialize')
parser.add_argument('--optimized', required=True, help='drvjs binary supporting materialize --project')
parser.add_argument('--project', required=True, type=Path)
parser.add_argument('--filter', action='append', default=[])
parser.add_argument('--edit-file', required=True, type=Path, help='ordinary source file in the selected workspace')
parser.add_argument('--runs', type=int, default=7)
parser.add_argument('--command', nargs=argparse.REMAINDER, help='CI command to run in each workspace (place last)')
args = parser.parse_args()
if args.runs < 1 or args.edit_file.is_absolute() or '..' in args.edit_file.parts:
    parser.error('provide a positive run count and a project-relative source file')

rows = []
with tempfile.TemporaryDirectory(prefix='drvjs-workspace-') as scratch:
    root = Path(scratch).resolve()
    project = root / 'checkout'
    shutil.copytree(args.project, project, symlinks=True,
                    ignore=shutil.ignore_patterns('.git', 'node_modules', '.pnpm-store'))
    edit = project / args.edit_file
    if edit.resolve() != edit or not edit.is_file():
        parser.error('--edit-file must be a regular source file inside the checkout')
    original = edit.read_bytes()
    env = dict(os.environ, CI='true', DRVJS_JOBS='4')
    selectors = [arg for name in args.filter for arg in ['--filter', name]]
    for trial in range(args.runs + 1):
        # The warmup realizes both helpers, the archives and the installed layers.
        revision = original + f'\n// {root.name}-{trial}\n'.encode()
        edit.write_bytes(revision)
        order = ['baseline', 'optimized'] if trial % 2 == 0 else ['optimized', 'baseline']
        for mode in order:
            binary = getattr(args, mode)
            with tempfile.TemporaryDirectory(dir=root) as job:
                export, cache, work = (Path(job) / name for name in ['export', 'cache', 'work'])
                start = time.monotonic()
                subprocess.run([binary, 'export', '-C', project, '--cache-dependencies',
                                *selectors, '-o', export], env=env, stdout=subprocess.DEVNULL, check=True)
                exported = time.monotonic()
                layer = ['-A', 'dependencies'] if mode == 'optimized' else []
                subprocess.run(['nix-build', export, *layer, '--max-jobs', '4', '--cores', '4',
                                '-o', cache], env=env, stdout=subprocess.DEVNULL, check=True)
                built = time.monotonic()
                sources = ['--project', export / 'project'] if mode == 'optimized' else []
                subprocess.run([binary, 'materialize', cache, *sources, '-o', work], env=env, check=True)
                ready = time.monotonic()
                assert (work / args.edit_file).read_bytes() == revision
                if args.command:
                    job_env = dict(env, PATH=str(work / 'node_modules/.bin') + os.pathsep + env['PATH'])
                    subprocess.run(args.command, cwd=work, env=job_env, stdout=sys.stderr, check=True)
                end = time.monotonic() if args.command else ready
                if trial:
                    rows.append(dict(mode=mode, trial=trial, export=exported - start, nix=built - exported,
                                     materialize=ready - built, ready=ready - start,
                                     command=end - ready, total=end - start))

medians = {
    mode: {phase: statistics.median(r[phase] for r in rows if r['mode'] == mode)
           for phase in ['export', 'nix', 'materialize', 'ready', 'command', 'total']}
    for mode in ['baseline', 'optimized']
}
print(json.dumps(dict(project=str(args.project), filters=args.filter, edit_file=str(args.edit_file),
                      command=args.command or [], samples=rows, median_seconds=medians), indent=2))
