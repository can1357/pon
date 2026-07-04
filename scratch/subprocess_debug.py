import subprocess, sys

def mark(name):
    print('MARK', name, flush=True)

mark('echo start')
r = subprocess.run(['/bin/echo', 'hi'], capture_output=True, text=True)
print('echo done', repr(r.stdout), r.returncode, flush=True)
mark('cwd start')
r = subprocess.run(['sh', '-c', 'pwd'], cwd='/tmp', capture_output=True, text=True)
print('cwd done', repr(r.stdout), r.returncode, flush=True)
mark('exit start')
r = subprocess.run(['sh', '-c', 'exit 3'])
print('exit done', r.returncode, flush=True)
mark('missing start')
try:
    subprocess.run(['/no/such/bin'])
except BaseException as exc:
    print('missing caught', type(exc), getattr(exc, 'errno', None), repr(exc), flush=True)
else:
    print('missing none', flush=True)
