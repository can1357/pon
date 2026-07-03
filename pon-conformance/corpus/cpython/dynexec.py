print(eval("1+1"))
exec("x = 40")
print(eval("x + 2"))
expr_code = compile("x + 3", "<dynexpr>", "eval")
print(eval(expr_code))
stmt_code = compile("x = x + 4", "<dynstmt>", "exec")
exec(stmt_code)
print(x)
ns = {}
exec("def f():\n    return 9\nprint(f())", ns)
print(ns["f"]())
exec(compile("3 + 4", "<single>", "single"))
mod = __import__("import_support")
print(mod.name)

def caught(label, fn, exc):
    try:
        fn()
        print(label, "no-error")
    except exc as err:
        print(label, "caught", type(err).__name__)


caught("exec-int", lambda: exec(123), TypeError)
caught("eval-int", lambda: eval(123), TypeError)
try:
    compile(b"# coding: ascii\n\xff\n", "bad_ascii.py", "exec")
except SyntaxError as err:
    print("compile-ascii", type(err).__name__, "'ascii' codec" in str(err), "ordinal not in range(128)" in str(err))
try:
    compile(b"\xff\n", "bad_utf8.py", "exec")
except SyntaxError as err:
    print("compile-utf8", type(err).__name__, "Non-UTF-8 code starting with" in str(err), "line 1" in str(err))
