# Generator defs in shapes where lowering visits siblings out of source
# order or re-lowers a statement list: child-scope pairing must key on the
# defining construct, not discovery order (test.support.os_helper's
# subst_drive shape: same-named defs in `except` and `else`).

# Distilled os_helper shape: plain def in the handler, generator def with the
# same name in `else`.  Positional pairing swapped the two scopes, so the
# generator body kept a raw yield marker past the state-machine transform.
try:
    pass
except ImportError:
    def subst(path):
        raise RuntimeError('unavailable')
else:
    def subst(path):
        yield path
        yield path * 2

print("else-generator", list(subst(3)))


# Mirror: the generator sits in the handler, the plain def in `else`.
try:
    raise ImportError('forced')
except ImportError:
    def mirrored(path):
        yield path
        yield 'handler'
else:
    def mirrored(path):
        return [path, 'else']

print("handler-generator", list(mirrored(4)))


# except* routes through separate try-star lowering with the same clause order.
try:
    pass
except* ValueError:
    def starred(path):
        raise RuntimeError('unavailable')
else:
    def starred(path):
        yield path
        yield path + 1

print("starred-else-generator", list(starred(7)))


# Same-kind same-name children with swapped inner scope trees: the else
# lambda owns a genexpr child, the handler lambda owns none.
try:
    pass
except ImportError:
    swapped = lambda: 'handler'
else:
    swapped = lambda: (x * x for x in (1, 2, 3))

print("lambda-genexpr", list(swapped()))


# `finally` bodies are inlined once per departing edge, so their defs are
# claimed more than once during lowering; CPython executes the body once per
# dynamic pass.
def finally_def(flag):
    try:
        if flag:
            return 'early'
    finally:
        def g():
            yield 'from-finally'
        print("finally-pass", list(g()))
    return 'late'

print("finally-return", finally_def(True))
print("finally-fallthrough", finally_def(False))


# Comprehensions in an inlined finally claim their child scope per copy too.
def finally_comp():
    try:
        return 'done'
    finally:
        squares = [n * n for n in range(3)]
        print("finally-comp", squares)

print("finally-comp-return", finally_comp())


# Same-named classes across handler and else pair by span as well.
try:
    pass
except ImportError:
    class Picked:
        label = 'handler'
else:
    class Picked:
        label = 'else'

        def tag(self):
            yield self.label

print("class-else", list(Picked().tag()))


# Neighbors that already paired correctly must stay put: conditional defs in
# if/else arms and a yield nested under `with` blocks.
class _Ctx:
    def __enter__(self):
        return 'ctx'

    def __exit__(self, *exc):
        return False


if hasattr(int, 'missing_attribute'):
    def cond_gen():
        yield 'if-arm'
else:
    def cond_gen():
        with _Ctx() as outer:
            with _Ctx() as inner:
                yield (outer, inner)

print("if-else-with", list(cond_gen()))
