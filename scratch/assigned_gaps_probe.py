mods = ['_colorize','_collections','_csv','_pickle','itertools','_warnings','warnings','_blake2','_sha1','_sha2','_md5','hashlib','gc','_thread','importlib','pkgutil']
for name in mods:
    try:
        m = __import__(name)
        print(name, 'OK', len(dir(m)))
    except Exception as exc:
        print(name, 'ERR', type(exc).__name__, exc)
