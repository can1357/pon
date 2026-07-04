import pathlib
def show(label, fn):
    try: print(label, "=>", fn())
    except BaseException as e: print(label, "ERR", type(e).__name__, e)
p = pathlib.Path("/tmp")
show("str(Path)", lambda: str(p))
show("Path.__str__()", lambda: p.__str__())
show("fspath(Path)", lambda: __import__('os').fspath(p))
show("Path.__fspath__()", lambda: p.__fspath__())
q = pathlib.Path("a/b")
show("str(rel Path)", lambda: str(q))
