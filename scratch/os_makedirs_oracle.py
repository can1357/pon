import os, tempfile, shutil
base = tempfile.mkdtemp(prefix='pon_os_oracle_')
try:
    f = os.path.join(base, 'file')
    open(f, 'w').close()
    for path, kwargs in [(f, {'exist_ok': True}), (f, {'exist_ok': False}), (f + '/child', {'exist_ok': True})]:
        try:
            os.makedirs(path, **kwargs)
        except Exception as exc:
            print(path.replace(base, '<base>'), kwargs, type(exc).__name__, getattr(exc, 'errno', None))
        else:
            print(path.replace(base, '<base>'), kwargs, 'OK')
    dot = os.path.join(base, 'dotdir', '.')
    try:
        os.makedirs(dot)
    except Exception as exc:
        print('dot', type(exc).__name__, getattr(exc, 'errno', None), os.path.isdir(os.path.join(base, 'dotdir')))
    else:
        print('dot', 'OK', os.path.isdir(os.path.join(base, 'dotdir')))
finally:
    shutil.rmtree(base, ignore_errors=True)
