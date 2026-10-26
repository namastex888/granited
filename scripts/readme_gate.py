#!/usr/bin/env python3
"""CI check: the README and its diagram claim only what `model: "auto"` does.

`auto` may be offered only on endpoints whose capability the named example profile,
configs/examples/auto.json, lists under auto.prefer. Choosing model size or effort by need
(phase C2 of reports/2026-10-26-auto-v0-spec.md) is not built, so no text may claim it.
"""
import json
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[1]
CAPABILITY = {'/v1/chat/completions': 'chat', '/v1/embeddings': 'embeddings', '/v1/forecast': 'forecast'}
C2_CLAIMS = [r'how hard it should think', r'its size', r'size and effort', r'size · effort', r'think: (low|full)',
             r'picks the device, model size']


def main():
    covered = set(json.loads((ROOT / 'configs/examples/auto.json').read_text())['auto']['prefer'])
    readme = (ROOT / 'README.md').read_text()
    problems = []
    for line in readme.splitlines():
        row = re.match(r'\| `(/v1/[^`]+)` \|(.*)\|', line)
        if row and 'auto' in row.group(2):
            capability = CAPABILITY.get(row.group(1))
            if capability not in covered:
                problems.append(f'README offers auto on {row.group(1)}, which configs/examples/auto.json does not cover')
    for name in ('README.md', 'docs/how-it-works.svg'):
        text = (ROOT / name).read_text()
        problems += [f'{name} claims "{m.group(0)}", which auto does not do yet' for p in C2_CLAIMS for m in re.finditer(p, text)]
    for problem in problems:
        print('FAIL ' + problem)
    print('PASS README matches what auto does' if not problems else '')
    sys.exit(1 if problems else 0)


if __name__ == '__main__':
    main()
