from __future__ import annotations

from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
import subprocess

ROOT = Path('/work/pon')
PON = Path('/Users/can/.cache/cargo-target/debug/pon-cli')
TMP = ROOT / 'tmp'
TARGETS = [
    ('concurrent.futures', TMP / 'stress_concurrent_futures.py'),
    ('importlib.resources', TMP / 'stress_importlib_resources.py'),
    ('logging.handlers', TMP / 'stress_logging_handlers.py'),
]
for name, path in TARGETS:
    path.write_text(f'import {name}\n')

def run_one(path: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(PON), str(path)],
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )

failures: list[tuple[int, int, str, subprocess.CompletedProcess[str]]] = []
rounds = 5
width = 12
for round_idx in range(rounds):
    with ThreadPoolExecutor(max_workers=width) as executor:
        jobs = []
        for slot in range(width):
            name, path = TARGETS[slot % len(TARGETS)]
            jobs.append((slot, name, executor.submit(run_one, path)))
        for slot, name, future in jobs:
            result = future.result()
            if result.returncode != 0 or result.stdout or result.stderr:
                failures.append((round_idx, slot, name, result))
    print(f'round {round_idx} ok')

print(f'processes {rounds * width}')
print(f'failures {len(failures)}')
for round_idx, slot, name, result in failures[:10]:
    print(f'FAIL round={round_idx} slot={slot} import={name} code={result.returncode}')
    if result.stdout:
        print('STDOUT:', result.stdout[-500:])
    if result.stderr:
        print('STDERR:', result.stderr[-1500:])
raise SystemExit(1 if failures else 0)
