import mesonpy
try:
    print("WHEEL", mesonpy.build_wheel("/tmp/np_wheel_out"))
except BaseException as e:
    tb = e.__traceback__
    frames = []
    while tb:
        f = tb.tb_frame
        frames.append(f"{f.f_code.co_filename}:{tb.tb_lineno} {f.f_code.co_name}")
        tb = tb.tb_next
    print("=== FRAMES ===")
    for fr in frames: print(fr)
    print("ERR", type(e).__name__, e)
