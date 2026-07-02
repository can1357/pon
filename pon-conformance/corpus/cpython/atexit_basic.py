import atexit


def first():
    print("first-registered")


def second(tag):
    print("second-registered", tag)


def late():
    print("late (must not run)")


def dropped():
    print("dropped (must not run)")


# Bookkeeping surface.
r = atexit.register(first)
print("register returns func", r is first)
atexit.register(second, "arg")
print("ncallbacks", atexit._ncallbacks())
atexit.register(dropped)
atexit.unregister(dropped)
print("ncallbacks after unregister", atexit._ncallbacks())


def reentrant():
    # Callbacks registered while exit callbacks run are not invoked
    # (observed CPython 3.14 behavior).
    print("reentrant")
    atexit.register(late)


atexit.register(reentrant)
print("main done")
# At exit: reentrant, second-registered arg, first-registered (LIFO).
