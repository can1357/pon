import pathlib, collections, tomllib
def show(label, fn):
    try: print(label, "=>", fn())
    except Exception as e: print(label, "ERR", type(e).__name__, e)
show("Path.absolute", lambda: str(pathlib.Path("/tmp").absolute()))
show("defaultdict(list)", lambda: type(collections.defaultdict(list)).__name__)
def dd():
    d = collections.defaultdict(list); d['k'].extend([1,2]); return dict(d)
show("defaultdict[k].extend", dd)
show("Path truediv", lambda: str(pathlib.Path("/tmp") / "x.ini"))
show("Path.joinpath", lambda: str(pathlib.Path("/tmp").joinpath("a")))
show("Path.read_text", lambda: pathlib.Path("/tmp/np_wheel_out").joinpath("nope").read_text() if False else "skip")
show("tomllib.loads", lambda: tomllib.loads('x = 1')['x'])
