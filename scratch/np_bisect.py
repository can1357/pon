import mesonpy, functools
def trace(obj, name):
    orig = getattr(obj, name, None)
    if not callable(orig): return
    @functools.wraps(orig)
    def w(*a, **k):
        print("ENTER", name, flush=True)
        try:
            r = orig(*a, **k)
            print("EXIT ", name, flush=True)
            return r
        except BaseException as e:
            print("FAIL ", name, type(e).__name__, repr(str(e)), flush=True)
            raise
    setattr(obj, name, w)
for fn in ["_get_meson_command","_map_to_wheel","_compile_patterns","_validate_pyproject_config","_validate_config_settings"]:
    trace(mesonpy, fn)
for m in ["__init__","_configure","_run","build","wheel"]:
    trace(mesonpy.Project, m)
try:
    mesonpy.build_wheel("/tmp/np_wheel_out")
    print("BUILT")
except BaseException as e:
    print("TOP", type(e).__name__, repr(str(e)))
