import subprocess
print('make popen', flush=True)
p = subprocess.Popen(['/bin/echo','hi'], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
print('made', p.pid, p.stdout, p.stderr, flush=True)
try:
    out, err = p.communicate()
    print('communicated', repr(out), repr(err), p.returncode, flush=True)
except BaseException as exc:
    print('caught', type(exc).__name__, repr(exc), getattr(exc, 'args', None), flush=True)
