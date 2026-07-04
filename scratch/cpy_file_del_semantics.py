import tempfile, os
p = tempfile.NamedTemporaryFile(delete=False)
path = p.name
p.close()
try:
    with open(path, 'w+', encoding='utf-8') as f:
        for name in ['x','mode','read','name','closed']:
            try:
                setattr(f, name, 'v')
            except Exception as e:
                print(name, 'seterr', type(e).__name__, str(e))
            try:
                delattr(f, name)
                print(name, 'del-ok')
            except Exception as e:
                print(name, 'del-err', type(e).__name__, str(e))
            print(name, 'has', hasattr(f, name))
finally:
    os.unlink(path)
