mods = ["subprocess", "sysconfig", "tarfile", "tempfile", "importlib.machinery", "tomllib", "gzip", "importlib.resources"]
for name in mods:
    try:
        __import__(name)
        print(name, "ok")
    except Exception as exc:
        print(name, type(exc).__name__, exc)
