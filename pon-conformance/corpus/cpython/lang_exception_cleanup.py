# Derived from CPython v3.14.0 Lib/test/test_exceptions.py topics (PSF license).

def local_cleanup():
    try:
        raise ValueError("local")
    except ValueError as problem:
        print("local inside", type(problem).__name__, problem.args[0])
    try:
        problem
    except UnboundLocalError as exc:
        print("local cleanup", type(exc).__name__)


def shadowed_cleanup():
    name = "outer"
    try:
        raise LookupError("shadow")
    except LookupError as name:
        print("shadow inside", type(name).__name__, name.args[0])
    try:
        name
    except UnboundLocalError as exc:
        print("shadow cleanup", type(exc).__name__)


def saved_exception_survives():
    holder = []
    try:
        raise RuntimeError("saved")
    except RuntimeError as err:
        holder.append(err)
        print("saved inside", type(err).__name__)
    print("saved later", type(holder[0]).__name__, holder[0].args[0])


local_cleanup()
shadowed_cleanup()
saved_exception_survives()
try:
    raise KeyError("module")
except KeyError as module_error:
    print("module inside", type(module_error).__name__, module_error.args[0])
try:
    module_error
except NameError as exc:
    print("module cleanup", type(exc).__name__)
