import pathlib, subprocess, traceback

def wrapper():
    try:
        subprocess.run(['/bin/echo', 'ok'], cwd=pathlib.Path('/tmp'))
    except BaseException as exc:
        print('inner', repr(exc), type(exc))
        raise

try:
    wrapper()
except BaseException as exc:
    print('outer', repr(exc), type(exc))
    traceback.print_exc()
    raise
