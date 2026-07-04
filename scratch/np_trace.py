import sys, mesonpy
log = []
def tracer(frame, event, arg):
    if event == 'call':
        c = frame.f_code
        log.append((c.co_filename.split('/')[-1], c.co_firstlineno, c.co_name))
    return tracer
sys.settrace(tracer)
try:
    mesonpy.build_wheel("/tmp/np_wheel_out")
    print("BUILT OK")
except BaseException as e:
    sys.settrace(None)
    print("=== LAST 30 CALLS ===")
    for entry in log[-30:]: print(entry)
    print("ERR", type(e).__name__, e)
