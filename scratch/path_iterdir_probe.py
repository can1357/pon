import pathlib
for name in ['PKG-INFO', 'dist']:
    print('iter', name)
    try:
        print(any(pathlib.Path(name).iterdir()))
    except BaseException as e:
        print(type(e).__name__, str(e))
        raise
