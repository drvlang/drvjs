#!/usr/bin/env python3
"""Alternate forced Nix rebuilds with already-realized tools and archives.

Includes evaluation, assembly, NAR checking and daemon overhead. Excludes export,
downloads and compilation of drvjs. --check also verifies reproducibility.
"""
import argparse
import json
from pathlib import Path
import shutil
import statistics
import subprocess
import tempfile
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--before', type=Path)
parser.add_argument('--after', type=Path)
parser.add_argument('--case', action='append', default=[], help='Named export NAME=PATH; repeat to compare several modes instead of --before/--after')
parser.add_argument('--fresh', action='store_true', help='Force fresh assembly including dependency-layer preparation; tools and downloads remain cached')
parser.add_argument('--source-edits', action='store_true', help='Rebuild copied exports after ordinary source edits, keeping installed layers cached')
parser.add_argument('--runs', type=int, default=5)
parser.add_argument('--out', type=Path, required=True)
args = parser.parse_args()
if args.runs < 1:
    parser.error('--runs must be positive')
if args.fresh and args.source_edits:
    parser.error('--fresh and --source-edits are mutually exclusive')
if args.case:
    if args.before or args.after:
        parser.error('--case cannot be combined with --before/--after')
    cases = {}
    for value in args.case:
        name, separator, path = value.partition('=')
        if not separator or not name or not path or name in cases:
            parser.error('--case needs a unique NAME=PATH')
        cases[name] = Path(path)
else:
    if not args.before or not args.after:
        parser.error('provide --before and --after, or repeat --case')
    cases = dict(before=args.before, after=args.after)
args.out.mkdir(parents=True, exist_ok=False)
if args.source_edits:
    copies = {}
    for mode, path in cases.items():
        copies[mode] = args.out / 'exports' / mode
        shutil.copytree(path, copies[mode])
    cases = copies
commands = {mode: ['nix-build', str(path.resolve()), '--no-out-link',
                  '--max-jobs', '4', '--cores', '4']
            for mode, path in cases.items()}
outputs = {}
for mode, command in commands.items():
    outputs[mode] = subprocess.check_output(command, text=True).strip()
rows = []
nonce = Path(tempfile.mkdtemp(prefix='drvjs-rebuild-')).name
# Only the unique name is needed; leave no directory behind.
Path(tempfile.gettempdir(), nonce).rmdir()
for trial in range(args.runs):
    order = list(commands) if trial % 2 == 0 else list(reversed(commands))
    if len(order) > 2:
        offset = trial % len(order)
        order = order[offset:] + order[:offset]
    for mode in order:
        marker = '.drvjs-benchmark-source-edit'
        marker_content = f'{nonce}-{trial}\n'
        if args.source_edits:
            (cases[mode] / 'project' / marker).write_text(marker_content)
        if args.fresh:
            expression = '''{ export, nonce }: let
              graph = import export {};
              fresh = g: args: builtins.derivation {
                inherit (g) name system builder;
                outputs = g.outputs or [ "out" ];
                inherit args;
                PATH = g.PATH or "";
                preferLocalBuild = true;
                DRVJS_BENCHMARK_NONCE = nonce;
              };
              args = if builtins.head graph.args == "attach" then
                builtins.genList (i: if i == 2 then fresh graph.dependencies graph.dependencies.args
                  else builtins.elemAt graph.args i) (builtins.length graph.args)
                else graph.args;
            in fresh graph args'''
            command = ['nix-build', '--expr', expression, '--argstr', 'export', str(cases[mode].resolve()),
                       '--argstr', 'nonce', f'{nonce}-{trial}', '--no-out-link', '--max-jobs', '4', '--cores', '4']
        elif args.source_edits:
            command = commands[mode]
        else:
            command = commands[mode] + ['--check']
        with (args.out / f'{mode}-{trial}.log').open('w') as log:
            started = time.monotonic()
            result = subprocess.run(command, text=True, stdout=subprocess.PIPE,
                                    stderr=log, check=True)
            elapsed = time.monotonic() - started
        if args.source_edits:
            assert (Path(result.stdout.strip()) / marker).read_text() == marker_content
        elif not args.fresh:
            assert result.stdout.strip() == outputs[mode]
        rows.append(dict(mode=mode, trial=trial, seconds=elapsed, command=command, output=result.stdout.strip()))
        print(mode, trial, round(elapsed, 4), flush=True)
        report = dict(outputs=outputs, samples=rows, medians={
            mode: statistics.median(row['seconds'] for row in rows if row['mode'] == mode)
            for mode in {row['mode'] for row in rows}})
        (args.out / 'results.json').write_text(json.dumps(report, indent=2) + '\n')
