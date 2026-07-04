import subprocess, traceback
try:
    subprocess.run(['/bin/echo', 'x'], cwd=object())
except Exception as e:
    print(type(e).__name__, str(e))
