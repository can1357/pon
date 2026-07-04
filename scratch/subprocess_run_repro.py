import pathlib, subprocess
print('before')
r = subprocess.run(['/bin/echo', 'ok'], cwd=pathlib.Path('/tmp'))
print('after', r.returncode)
