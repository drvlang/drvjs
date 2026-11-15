#!/usr/bin/env python3
"""Measure first publication and incremental source-update cache payloads.

Each case has its own fresh local file cache. Every publication includes the
full runtime closure; unchanged NARs already in that cache are reused. Original
exports are copied before editing. Local publication time is not network time.
Tools, archives and installed dependency layers are realized before timing.
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
parser.add_argument('--case', action='append', required=True, help='NAME=EXPORT')
parser.add_argument('--runs', type=int, default=5)
parser.add_argument('--level', type=int, default=3)
parser.add_argument('--out', type=Path, required=True)
args = parser.parse_args()
if args.runs < 1:
    parser.error('--runs must be positive')
cases = {}
for case in args.case:
    name, separator, path = case.partition('=')
    if not separator or not path or not name or name in {'.', '..'} or Path(name).name != name or name in cases:
        parser.error('--case must be a unique NAME=EXPORT with a simple name')
    cases[name] = Path(path).resolve()
args.out.mkdir(parents=True, exist_ok=False)
rows = []
with tempfile.TemporaryDirectory(prefix='drvjs-transfer-update-') as scratch:
    scratch = Path(scratch)
    for name, export in cases.items():
        shutil.copytree(export, scratch / name / 'export')
        (scratch / name / 'cache').mkdir()

    def build(name):
        return ['nix-build', str(scratch / name / 'export'), '--no-out-link', '--max-jobs', '4', '--cores', '4']

    # Realize all helpers/archives/layers outside measured source rebuilds.
    for name in cases:
        subprocess.run(build(name), check=True, stdout=subprocess.DEVNULL)
    for trial in range(args.runs + 1):
        order = list(cases) if trial % 2 == 0 else list(reversed(cases))
        for name in order:
            marker = scratch / name / 'export/project/.drvjs-transfer-benchmark'
            if trial:
                marker.write_text(f'{scratch.name}-{trial}\n')
            command = build(name)
            with (args.out / f'{name}-{trial}.log').open('w') as log:
                start = time.monotonic()
                output = subprocess.check_output(command, text=True, stderr=log).strip()
                build_seconds = time.monotonic() - start
                if trial:
                    assert (Path(output) / marker.name).read_text() == marker.read_text()
                cache = scratch / name / 'cache'
                before = {p.name for p in (cache / 'nar').glob('*') if p.is_file()}
                publish = ['nix', 'copy', '--to', cache.as_uri() + f'?compression=zstd&compression-level={args.level}', output]
                start = time.monotonic()
                subprocess.run(publish, check=True, stdout=log, stderr=log)
                publish_seconds = time.monotonic() - start
                new = [p for p in (cache / 'nar').glob('*') if p.is_file() and p.name not in before]
            info = json.loads(subprocess.check_output(['nix', 'path-info', '--json', '--recursive', output], text=True))
            values = info if isinstance(info, list) else list(info.values())
            closure = sorted(p['path'] for p in info) if isinstance(info, list) else sorted(info)
            rows.append(dict(case=name, trial=trial, phase='initial' if not trial else 'source-update',
                             output=output, closure=closure, closure_paths=len(values), closure_nar_bytes=sum(p['narSize'] for p in values),
                             new_payloads=len(new), transferred_bytes=sum(p.stat().st_size for p in new),
                             build_seconds=build_seconds, publish_seconds=publish_seconds,
                             build_command=command, publish_command=publish))
            report = dict(exports={name: str(path) for name, path in cases.items()}, level=args.level, samples=rows, source_update_medians={
                name: {key: statistics.median(r[key] for r in rows if r['case'] == name and r['trial'])
                       for key in ['transferred_bytes', 'build_seconds', 'publish_seconds']}
                for name in cases if any(r['case'] == name and r['trial'] for r in rows)})
            (args.out / 'results.json').write_text(json.dumps(report, indent=2) + '\n')
            print(json.dumps({k: v for k, v in rows[-1].items() if not k.endswith('command')}), flush=True)
