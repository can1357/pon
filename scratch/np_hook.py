import mesonpy, traceback
for hook in ("get_requires_for_build_wheel",):
    fn = getattr(mesonpy, hook)
    try:
        print(hook, "=>", fn())
    except Exception:
        print("EXC in", hook)
        traceback.print_exc()
