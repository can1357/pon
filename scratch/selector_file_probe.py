import os, selectors, io
r, w = os.pipe()
f = io.open(r, 'rb', -1)
os.write(w, b'x')
try:
    print('fileno attr', f.fileno, flush=True)
    print('fileno call', f.fileno(), flush=True)
    sel = selectors.DefaultSelector()
    print('registering file', flush=True)
    key = sel.register(f, selectors.EVENT_READ)
    print('registered', key, flush=True)
    print('select', sel.select(0), flush=True)
except BaseException as exc:
    print('caught', type(exc).__name__, repr(exc), getattr(exc, 'args', None), flush=True)
finally:
    try: f.close()
    except Exception: pass
    os.close(w)
