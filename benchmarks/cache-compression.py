#!/usr/bin/env python3
"""Measure Nix binary-cache bytes and publication wall time without changing files.

Uses fresh local file caches. No remote latency or download time is simulated.
The output's full runtime closure is copied, so references cannot hide bytes.
"""
import argparse
import json
from pathlib import Path
import subprocess
import tempfile
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('output', type=Path)
parser.add_argument('--levels', type=int, nargs='+', default=[3, 9, 19])
parser.add_argument('--runs', type=int, default=3)
parser.add_argument('--out', type=Path, required=True)
args = parser.parse_args()
if args.runs < 1:
    parser.error('--runs must be positive')
output = str(args.output.resolve())
paths = subprocess.check_output(['nix-store', '-qR', output], text=True).splitlines()
rows = []
for trial in range(args.runs):
    for level in args.levels if trial % 2 == 0 else list(reversed(args.levels)):
        with tempfile.TemporaryDirectory(prefix='drvjs-compression-') as temporary:
            cache = Path(temporary)
            command = ['nix', 'copy', '--to', cache.as_uri() + f'?compression=zstd&compression-level={level}', output]
            start = time.monotonic()
            subprocess.run(command, check=True, stdout=subprocess.DEVNULL)
            elapsed = time.monotonic() - start
            sizes = []
            for path in paths:
                key = Path(path).name.split('-')[0]
                fields = dict(line.split(': ', 1) for line in (cache / (key + '.narinfo')).read_text().splitlines() if ': ' in line)
                sizes.append(int(fields['FileSize']))
            rows.append(dict(trial=trial, level=level, closure_bytes=sum(sizes), publish_seconds=elapsed))
            args.out.write_text(json.dumps(dict(output=output, closure_paths=paths, samples=rows), indent=2) + '\n')
            print(json.dumps(rows[-1]), flush=True)
