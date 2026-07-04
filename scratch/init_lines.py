import pathlib, collections, tomllib, os
source_dir = pathlib.Path(os.getcwd())
build_dir = "/tmp/np_build_test"
try:
    _source_dir = pathlib.Path(source_dir).absolute(); print("672 ok")
    _build_dir = pathlib.Path(build_dir).absolute(); print("673 ok")
    _mnf = _build_dir / 'x.ini'; print("675 ok")
    _margs = collections.defaultdict(list); print("677 ok")
    j = _source_dir.joinpath('pyproject.toml'); print("681a joinpath ok")
    txt = j.read_text(encoding='utf-8'); print("681b read_text ok", len(txt))
    pyproject = tomllib.loads(txt); print("681c tomllib ok")
except BaseException as e:
    print("FAILED:", type(e).__name__, repr(str(e)))
