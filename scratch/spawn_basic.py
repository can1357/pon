import subprocess
print('before')
r = subprocess.run(['/bin/echo','hi'])
print('after', r.returncode)
r = subprocess.run(['sh','-c','exit 3'])
print('exit', r.returncode)
try:
    subprocess.run(['/no/such/bin'])
except FileNotFoundError as exc:
    print('missing', type(exc).__name__, exc.errno)
