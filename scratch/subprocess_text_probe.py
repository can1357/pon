import subprocess
p = subprocess.run(["/bin/echo", "hello"], capture_output=True, text=True)
print(p.returncode, repr(p.stdout), repr(p.stderr))
