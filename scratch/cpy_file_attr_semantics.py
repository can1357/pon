import tempfile, os
p = tempfile.NamedTemporaryFile(delete=False)
path = p.name
p.close()
try:
    with open(path, 'w+', encoding='utf-8') as f:
        for name, value in [('x', 1), ('name', 'dyn-name'), ('mode', 'dyn-mode'), ('read', 'dyn-read'), ('closed', 'dyn-closed')]:
            try:
                setattr(f, name, value)
                print(name, 'set-ok', repr(getattr(f, name)))
            except Exception as e:
                print(name, 'set-err', type(e).__name__, str(e))
        print('__dict__', sorted(f.__dict__.items()))
finally:
    os.unlink(path)
