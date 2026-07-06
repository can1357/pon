from __future__ import annotations

from pathlib import Path
import subprocess

ROOT = Path('/work/pon')
PROBE = ROOT / 'tmp' / 'abc_chain_probe.py'
PON = Path('/Users/can/.cache/cargo-target/debug/pon')

def run(cmd: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(cmd, cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)

cpy = run(['python3.14', str(PROBE)])
pon = run([str(PON), str(PROBE)])
print(f'cpython code={cpy.returncode}')
print(repr(cpy.stdout))
if cpy.stderr:
    print('cpython stderr:', cpy.stderr[-1000:])
print(f'pon code={pon.returncode}')
print(repr(pon.stdout))
if pon.stderr:
    print('pon stderr:', pon.stderr[-1000:])
print(f'stdout_identical={cpy.stdout == pon.stdout}')
raise SystemExit(0 if cpy.returncode == 0 and pon.returncode == 0 and cpy.stdout == pon.stdout else 1)
