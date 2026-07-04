import mesonpy
try:
    print("REQ", mesonpy.get_requires_for_build_wheel())
except BaseException as e:
    tb = e.__traceback__
    while tb:
        f = tb.tb_frame
        print(f"{f.f_code.co_filename}:{tb.tb_lineno} in {f.f_code.co_name}")
        tb = tb.tb_next
    print("ERR", type(e).__name__, e)
