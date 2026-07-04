import pathlib, subprocess, traceback
print('before')
try:
    subprocess.run(['/bin/echo', 'ok'], cwd=pathlib.Path('/tmp'))
except BaseException:
    print('caught')
    traceback.print_exc()
    raise
