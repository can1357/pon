import os, selectors, sys
print('selector', selectors.DefaultSelector, flush=True)
r, w = os.pipe()
print('pipe', r, w, flush=True)
os.write(w, b'x')
try:
    sel = selectors.DefaultSelector()
    print('made', sel, flush=True)
    key = sel.register(r, selectors.EVENT_READ)
    print('registered', key, flush=True)
    ready = sel.select(0)
    print('ready', ready, flush=True)
    sel.unregister(r)
    print('unregistered', flush=True)
except BaseException as exc:
    print('caught', type(exc).__name__, repr(exc), getattr(exc, 'args', None), flush=True)
finally:
    os.close(r); os.close(w)
