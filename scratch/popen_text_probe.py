import subprocess
for text in (False, True):
    print('case', text, flush=True)
    try:
        p = subprocess.Popen(['/bin/echo','hi'], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=text)
        out, err = p.communicate()
        print('ok', repr(out), repr(err), p.returncode, flush=True)
    except BaseException as exc:
        print('caught', type(exc).__name__, repr(exc), getattr(exc,'args',None), flush=True)
