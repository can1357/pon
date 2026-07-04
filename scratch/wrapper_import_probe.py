for name in ['sqlite3', 'ssl', 'ctypes', 'dbm']:
    try:
        __import__(name)
    except BaseException as exc:
        print('FAIL', name, type(exc).__name__, str(exc))
    else:
        print('OK', name)
