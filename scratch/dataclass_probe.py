for name in ['dataclasses','collections.abc','os','sys']:
    try:
        m = __import__(name, fromlist=['*'])
        print(name, 'OK')
    except Exception as exc:
        print(name, 'ERR', type(exc).__name__, exc)
