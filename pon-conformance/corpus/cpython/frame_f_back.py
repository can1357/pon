# frame.f_back chain: sys._getframe() captures the caller chain at call
# time — walking f_back yields each enclosing function's frame (co_name
# chain) and terminates with None past the module-toplevel frame; a depth
# argument starts the chain higher up, and a captured chain stays readable
# after the stack unwinds and the collector runs.
import gc
import sys


def names(frame):
    out = []
    while frame is not None:
        out.append(frame.f_code.co_name)
        frame = frame.f_back
    return out


def inner():
    return sys._getframe()


def outer():
    return inner()


print("chain", names(outer()))


def depth_one():
    return sys._getframe(1)


def depth_caller():
    return depth_one()


print("depth1", names(depth_caller()))

top = sys._getframe()
print("toplevel", top.f_code.co_name, top.f_back is None)

captured = outer()
gc.collect()
print("post-gc", names(captured), captured.f_back.f_code.co_name)
