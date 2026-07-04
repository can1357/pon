import types

def base(a, b=1, *, c=2):
    return (a, b, c)

# positional-only construction (should already work?)
try:
    f = types.FunctionType(base.__code__, base.__globals__)
    print("positional ok:", f(10))
except Exception as e:
    print("positional ERR", type(e).__name__, e)

# keyword args like annotationlib uses
try:
    f = types.FunctionType(
        base.__code__,
        base.__globals__,
        closure=None,
        argdefs=base.__defaults__,
        kwdefaults=base.__kwdefaults__,
    )
    print("kwargs ok:", f(10))
except Exception as e:
    print("kwargs ERR", type(e).__name__, e)

# custom globals dict
try:
    g = {"__name__": "custom", "MARKER": 99}
    src = compile("def h():\n    return MARKER\n", "<s>", "exec")
    ns = {}
    exec(src, g, ns)
    h = ns["h"]
    f2 = types.FunctionType(h.__code__, g)
    print("custom globals ok:", f2(), "globals MARKER:", f2.__globals__.get("MARKER"))
except Exception as e:
    print("custom globals ERR", type(e).__name__, e)
