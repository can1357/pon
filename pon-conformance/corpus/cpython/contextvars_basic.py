from contextvars import ContextVar, Token, copy_context

v = ContextVar("v", default="dflt")
print("default", v.get())
t1 = v.set("a")
t2 = v.set("b")
print("after sets", v.get())
print("t2 old", t2.old_value)
print("t1 old missing", t1.old_value is Token.MISSING)
v.reset(t2)
print("after reset t2", v.get())
v.reset(t1)
print("after reset t1", v.get())

w = ContextVar("w")
try:
    w.get()
except LookupError:
    print("LookupError")
print("call default", w.get("fallback"))

w.set("outer")
ctx = copy_context()


def mutate():
    w.set("inner")
    return w.get()


print("run result", ctx.run(mutate))
print("outer after run", w.get())
print("copy sees", ctx.get(w))
print("fresh copy distinct", copy_context() is not ctx)

token = w.set("again")
try:
    v.reset(token)
except ValueError:
    print("ValueError different var")
w.reset(token)
try:
    w.reset(token)
except RuntimeError:
    print("RuntimeError reused token")
print("final", w.get())
