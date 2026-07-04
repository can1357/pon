import subprocess
print('start', flush=True)
try:
    r = subprocess.run(['/bin/echo','hi'], capture_output=True, text=True)
    print('ok', repr(r.stdout), r.returncode, flush=True)
except BaseException as exc:
    print('caught', type(exc).__name__, repr(exc), flush=True)
    print('args', getattr(exc, 'args', None), flush=True)
