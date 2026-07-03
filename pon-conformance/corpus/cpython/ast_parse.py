# ast.parse / compile(source, filename, mode, PyCF_ONLY_AST): ruff-parsed
# source materialized as real _ast node trees (dynexec ast-parse hook +
# native _ast builder).  Round-trips are pinned by ast.dump equality, which
# also exercises the CPython 3.13+ default-omission class data (_field_types
# list markers, class-level None for optional fields).  Locations are
# CPython's convention — 1-based lineno, 0-based UTF-8 BYTE col_offset —
# derived from ruff byte ranges; decorated/async def/class statements are
# re-anchored onto their keyword.  This supersedes the ast_surface.py header
# note that ast.parse is "deliberately untested": the AST path is live now.
#
# Not modeled (documented divergences, not printed here): full typing
# generics in _field_types values, interned operator/context singletons
# (fresh instances per node), SyntaxError message text (type is pinned
# below), CPython's func_type parse mode.
import ast

# --- acceptance: x=1 round-trip -------------------------------------------
module = ast.parse('x=1')
print(type(module).__name__, len(module.body), type(module.body[0]).__name__)
print(ast.dump(module))

# --- representative statement/expression dumps ----------------------------
print(ast.dump(ast.parse("a, b = b, a = 1, 2")))
print(ast.dump(ast.parse("x += 1\ny: int = 5\nz: list[int]")))
print(ast.dump(ast.parse("del a, b[0], c.d")))
print(ast.dump(ast.parse(
    "if a:\n    x = 1\nelif b:\n    x = 2\nelif c:\n    x = 3\nelse:\n    x = 4")))
print(ast.dump(ast.parse("for i in range(3):\n    print(i)\nelse:\n    pass")))
print(ast.dump(ast.parse("while x:\n    break\nelse:\n    y = 1")))
print(ast.dump(ast.parse(
    "def f(a, b=1, /, c=2, *args, d, e=3, **kw) -> int:\n    return a")))
print(ast.dump(ast.parse(
    "@deco\n@deco2(1)\ndef h():\n    '''doc'''\n    yield 1\n    yield from x")))
print(ast.dump(ast.parse(
    "async def i():\n    await j()\n    async for q in r:\n        pass\n"
    "    async with s as t:\n        pass")))
print(ast.dump(ast.parse(
    "class C(Base, meta=M):\n    attr = 1\n    def m(self):\n        return super().m()")))
print(ast.dump(ast.parse("f(1, *a, k=2, **kw)")))
print(ast.dump(ast.parse("lambda a, b=2, *c, d=3, **e: a + d")))

# comprehensions: nested fors, filters, async, dict/set/generator forms
print(ast.dump(ast.parse("[x*y for x in a for y in b if x if y]")))
print(ast.dump(ast.parse("{k: v for k, v in items.items() if k}")))
print(ast.dump(ast.parse("{x for x in s}")))
print(ast.dump(ast.parse("(i async for i in aiter())")))
print(ast.dump(ast.parse("async def f():\n    return [i async for i in g()]")))

# constants: int/big-int/float/complex/bool/None/Ellipsis/bytes/str, u-kind,
# implicit concatenation merging (plain and mixed with f-strings)
print(ast.dump(ast.parse(
    "x = [1, 2.5, 1_000_000_000_000_000_000_000, 0xFFFF_FFFF_FFFF_FFFF_FFFF,"
    " 3j, True, None, ..., b'bytes', 'str']")))
print(ast.dump(ast.parse("u'unicode kind'")))
print(ast.dump(ast.parse("'implicit' 'concat'")))
print(ast.dump(ast.parse("s = 'a' f'{x}b' 'c'")))

# f-strings: conversion, nested format specs, and `=` debug text
print(ast.dump(ast.parse("t = f\"pre{x!r:>{w}}mid{y=}post{z:{a}.{b}f}\"")))
print(ast.dump(ast.parse("v = f'{x}'")))
print(ast.dump(ast.parse("w = f'{x = }'")))

# t-strings (PEP 750): TemplateStr with Interpolation.str source text
print(ast.dump(ast.parse('tpl = t"hi {x}"')))
print(ast.dump(ast.parse('tpl2 = t"a{v!s:>3}b"')))

# operators, comparisons, walrus, starred, subscripts and slices
print(ast.dump(ast.parse("a if b else c")))
print(ast.dump(ast.parse("not -+~x")))
print(ast.dump(ast.parse("a < b <= c != d in e not in g is h is not i")))
print(ast.dump(ast.parse("x = 1 @ m ** 2 // 3 % 4 << 5 >> 6 | 7 ^ 8 & 9")))
print(ast.dump(ast.parse("(a := 10)")))
print(ast.dump(ast.parse("*rest, last = seq")))
print(ast.dump(ast.parse("x[1] + x[1:2:3] + x[::2] + x[a, b:c] + x[..., None]")))
print(ast.dump(ast.parse("d = {**a, 1: 2}")))

# imports, scopes, exceptions, with, assert, match, PEP 695
print(ast.dump(ast.parse("import os, sys as system")))
print(ast.dump(ast.parse("from ..pkg import b as c, d")))
print(ast.dump(ast.parse("global g1, g2")))
print(ast.dump(ast.parse(
    "def q():\n    x = 1\n    def r():\n        nonlocal x\n        x = 2")))
print(ast.dump(ast.parse(
    "try:\n    pass\nexcept ValueError as e:\n    raise KeyError('k') from e\n"
    "except (TypeError, OSError):\n    raise\nelse:\n    a = 1\nfinally:\n    b = 2")))
print(ast.dump(ast.parse("try:\n    pass\nexcept* ValueError:\n    pass")))
print(ast.dump(ast.parse("with open('f') as fh, ctx() as (a, b):\n    pass")))
print(ast.dump(ast.parse("assert x, 'message'")))
print(ast.dump(ast.parse(
    "match p:\n"
    "    case 1 | 2:\n        pass\n"
    "    case 'lit':\n        pass\n"
    "    case None:\n        pass\n"
    "    case True:\n        pass\n"
    "    case [x, *rest]:\n        pass\n"
    "    case {'k': v, **extra}:\n        pass\n"
    "    case Point(0, y=0) as pt:\n        pass\n"
    "    case _:\n        pass")))
print(ast.dump(ast.parse("type Alias[T, *Ts, **P] = dict[T, P]")))
print(ast.dump(ast.parse("def gen[T: int, U: (int, str) = str]():\n    pass")))

# --- location spot checks ---------------------------------------------------
body = ast.parse("x = 1\n\ny = (\n    2)\n\n@d\ndef f():\n    pass\n").body
for node in body:
    print(node.lineno, node.col_offset, node.end_lineno, node.end_col_offset)
paren = body[1].value
print(paren.lineno, paren.col_offset, paren.end_lineno, paren.end_col_offset)

multibyte = ast.parse("é = 'ü'").body[0]
print(multibyte.lineno, multibyte.col_offset, multibyte.end_col_offset)
print(multibyte.value.col_offset, multibyte.value.end_col_offset)

chain = ast.parse("if a:\n    pass\nelif b:\n    pass\nelse:\n    pass").body[0]
inner = chain.orelse[0]
print(chain.lineno, chain.end_lineno, inner.lineno, inner.col_offset, inner.end_lineno)

# --- modes and errors --------------------------------------------------------
print("eval", ast.dump(ast.parse("x + 1", mode="eval")))
print("single", ast.dump(ast.parse("x = 1; y = 2", mode="single")))
try:
    ast.parse("def f(:")
except SyntaxError:
    print("SyntaxError")

# --- ast-consuming stdlib shapes --------------------------------------------
# ast.literal_eval walks Expression/Constant/containers/UnaryOp/BinOp.
print(ast.literal_eval("{'a': [1, 2.5, (True, None, ...)], 'b': b'x', 'c': 1+2j, 'd': -5}"))
# ast.get_docstring reads Module.body[0] Expr(Constant) — the docstring slot.
print(ast.get_docstring(ast.parse("'''module doc'''\nx = 1")))
# inspect._signature_fromstr-style consumption: FunctionDef.args split.
args = ast.parse("def sig(a, b=1, *rest, kw=None, **extra): pass").body[0].args
print([a.arg for a in args.args], len(args.defaults),
      [a.arg for a in args.kwonlyargs], [d and ast.dump(d) for d in args.kw_defaults],
      args.vararg.arg, args.kwarg.arg)
# generic tree walkers
print(sorted(type(n).__name__ for n in ast.walk(ast.parse("def f(x):\n    return [i for i in x]"))))
print([type(n).__name__ for n in ast.iter_child_nodes(ast.parse("x = 1").body[0])])
