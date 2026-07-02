# Distilled from Lib/traceback.py TracebackException.format_exception_only:
# a generator body that creates closure cells (locals captured by
# comprehensions and nested defs) and keeps using them across yield
# boundaries.  Baseline codegen used to cache the raw `pon_make_cell` SSA
# value for every later use, so resume paths entered from the dispatch
# br_table referenced a non-dominating definition and Cranelift rejected the
# body ("uses value from non-dominating inst").  Cells must spill to
# generator frame slots and reload on resume, exactly like locals.


# The original crash shape: captured `indent` used by comprehensions before
# and after plain yields, `yield from`, and a loop of `yield from`s.
def fmt(depth, show_group):
    indent = 3 * depth * ' '
    yield indent + 'head\n'
    formatted = 'TypeError: bad\nDetail: line'.split('\n')
    yield from [indent + l + '\n' for l in formatted]
    notes = ['n1\nn2', 'n3']
    for note in notes:
        yield from [indent + l + '!\n' for l in note.split('\n')]
    if show_group:
        def tail():
            return indent + 'tail\n'
        yield tail()


print(''.join(fmt(1, True)))
print(''.join(fmt(0, False)))


# Cell identity must survive suspension: a closure made before the first
# yield observes rebinding done after a resume (same cell object, not a
# fresh one reloaded per state).
def counter_gen():
    count = 0

    def peek():
        return count

    yield peek
    count = 10
    yield count
    count = count + 7
    yield count


g = counter_gen()
peek = next(g)
print(peek())
print(next(g), peek())
print(next(g), peek())


# Two cells created in the prologue, consumed on both sides of suspends,
# with `nonlocal` writing back through the cell after a resume.
def two_cells(a, b):
    x = a * 2
    y = b + '!'

    def bump():
        nonlocal x
        x += 1
        return y

    yield x
    yield bump()
    yield x
    yield [y + str(x) for _ in range(2)]


print(list(two_cells(3, 'z')))

# Independent frames: interleaved instances must not share cell slots.
g1 = two_cells(1, 'a')
g2 = two_cells(100, 'b')
print(next(g1), next(g2))
print(next(g1), next(g2))
print(next(g1), next(g2))
print(next(g1), next(g2))
