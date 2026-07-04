import pathlib, subprocess, sys
cmd=['/Users/can/.cache/cargo-target/debug/pon-cli', '/work/pon/tmp/child_ok.py']
print('before', repr(subprocess.run), type(subprocess.run))
r = subprocess.run(cmd, cwd=pathlib.Path('/tmp'))
print('after', r.returncode)
