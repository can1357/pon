mods = ['_collections_abc','collections.abc','typing','dataclasses']
for name in mods:
    try:
        m = __import__(name, fromlist=['*'])
        print(name, 'OK', [hasattr(m, x) for x in ('Callable','Iterator','Mapping')])
    except Exception as exc:
        print(name, 'ERR', type(exc).__name__, exc)
