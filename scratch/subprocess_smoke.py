import subprocess

r = subprocess.run(['/bin/echo', 'hi'], capture_output=True, text=True)
print('echo_stdout', repr(r.stdout), r.stdout == 'hi\n', r.returncode)

r = subprocess.run(['sh', '-c', 'pwd'], cwd='/tmp', capture_output=True, text=True)
print('cwd_stdout', repr(r.stdout), r.stdout.startswith('/tmp') or r.stdout.startswith('/private/tmp'), r.returncode)

r = subprocess.run(['sh', '-c', 'exit 3'])
print('exit_code', r.returncode, r.returncode == 3)

try:
    subprocess.run(['/no/such/bin'])
except FileNotFoundError as exc:
    print('missing_exec', type(exc).__name__, exc.errno)
else:
    print('missing_exec', 'NO_ERROR')
