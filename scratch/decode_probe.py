import subprocess
p = subprocess.Popen(['/bin/echo','hi'], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
print('encerr', p.stdout.encoding, p.stdout.errors, p.stderr.encoding, p.stderr.errors, flush=True)
out, err = p.communicate()
print('done', repr(out), repr(err), flush=True)
