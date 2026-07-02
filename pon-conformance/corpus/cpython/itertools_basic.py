import itertools
from itertools import (
    accumulate,
    batched,
    chain,
    combinations,
    compress,
    count,
    cycle,
    dropwhile,
    filterfalse,
    groupby,
    islice,
    pairwise,
    permutations,
    product,
    repeat,
    starmap,
    takewhile,
    zip_longest,
)

# --- chain / chain.from_iterable ------------------------------------------
print(list(chain([1, 2], (3, 4), "ab")))
print(list(chain()))
print(list(chain([], [5], [])))
print(list(chain.from_iterable([[1, 2], [], [3]])))
print(list(chain.from_iterable(["ab", "c"])))
print(list(itertools.chain.from_iterable([(True, False), (0,)])))
c = chain([9])
print(iter(c) is c)

# --- repeat ----------------------------------------------------------------
print(list(repeat("x", 4)))
print(list(repeat(7, 0)))
print(list(repeat(7, -3)))
print(list(repeat([1], 2)))
print(list(repeat(5, times=3)))
print(list(islice(repeat("inf"), 3)))

# --- starmap ---------------------------------------------------------------
print(list(starmap(lambda a, b: a * b, [(2, 3), (4, 5)])))
print(list(starmap(lambda: 42, [(), ()])))
print(list(starmap(lambda a, b, c: a + b + c, [[1, 2, 3], [4, 5, 6]])))
print(list(starmap(lambda a, b: (b, a), zip("ab", "cd"))))

# --- count (bounded through islice) ----------------------------------------
print(list(islice(count(), 5)))
print(list(islice(count(10), 4)))
print(list(islice(count(2, 3), 4)))
print(list(islice(count(step=5), 3)))
print(list(islice(count(start=1, step=-2), 4)))
print(list(islice(count(0.5, 0.25), 3)))

# --- cycle (bounded through islice) ----------------------------------------
print(list(islice(cycle([1, 2, 3]), 7)))
print(list(islice(cycle("ab"), 5)))
print(list(cycle([])))

# --- islice ----------------------------------------------------------------
data = list(range(10))
print(list(islice(data, 4)))
print(list(islice(data, 2, 6)))
print(list(islice(data, 2, None)))
print(list(islice(data, None)))
print(list(islice(data, 1, 9, 3)))
print(list(islice(data, 0, 0)))
print(list(islice(data, 20)))
print(list(islice(data, 12, 20)))
print(list(islice(data, None, None, 4)))
try:
    islice(data, -1)
except ValueError as exc:
    print("ValueError", exc)
try:
    islice(data, 1, 2, 0)
except ValueError as exc:
    print("ValueError", exc)


def gen():
    yield from range(100)


print(next(islice(gen(), 7, None)))
source = iter(range(10))
print(list(islice(source, 3)))
print(list(source))

# --- zip_longest -------------------------------------------------------------
print(list(zip_longest("ab", "xyz")))
print(list(zip_longest("ab", "xyz", fillvalue="-")))
print(list(zip_longest([1, 2], [3, 4])))
print(list(zip_longest()))
print(list(zip_longest([1, 2, 3])))
print(list(zip_longest("ab", "cd", "e", fillvalue=0)))

# --- product ----------------------------------------------------------------
print(list(product([1, 2], "ab")))
print(list(product([1, 2])))
print(list(product()))
print(list(product([1, 2], [])))
print(list(product([0, 1], repeat=2)))
print(list(product("ab", repeat=0)))
try:
    product([1], repeat=-1)
except ValueError as exc:
    print("ValueError", exc)

# --- permutations ------------------------------------------------------------
print(list(permutations("ABC", 2)))
print(list(permutations(range(3))))
print(list(permutations("AB", 0)))
print(list(permutations("AB", 3)))
print(list(permutations("ABCD", r=1)))
try:
    permutations("AB", -1)
except ValueError as exc:
    print("ValueError", exc)

# --- combinations -------------------------------------------------------------
print(list(combinations(range(4), 2)))
print(list(combinations("ABCD", 3)))
print(list(combinations("AB", 0)))
print(list(combinations("AB", 3)))
try:
    combinations("AB", -2)
except ValueError as exc:
    print("ValueError", exc)

# --- accumulate ---------------------------------------------------------------
print(list(accumulate([1, 2, 3, 4, 5])))
print(list(accumulate([])))
print(list(accumulate([3, 1, 2], lambda a, b: a * b)))
print(list(accumulate([1, 2, 3], func=lambda a, b: a - b)))
print(list(accumulate([1, 2, 3], initial=100)))
print(list(accumulate([], initial=9)))
print(list(accumulate("abc")))

# --- filterfalse ---------------------------------------------------------------
print(list(filterfalse(lambda x: x % 2, range(8))))
print(list(filterfalse(None, [0, 1, "", "a", [], [2]])))
print(list(filterfalse(lambda x: x < 3, [5, 1, 6])))

# --- takewhile / dropwhile ------------------------------------------------------
print(list(takewhile(lambda x: x < 5, [1, 4, 6, 3, 8])))
print(list(takewhile(lambda x: x < 5, [])))
tw = takewhile(lambda x: x < 2, [1, 9, 1])
print(list(tw))
print(list(tw))
print(list(dropwhile(lambda x: x < 5, [1, 4, 6, 3, 8])))
print(list(dropwhile(lambda x: x < 5, [9])))
print(list(dropwhile(lambda x: True, [1, 2])))

# --- compress --------------------------------------------------------------------
print(list(compress("ABCDEF", [1, 0, 1, 0, 1, 1])))
print(list(compress("ABC", [True, False])))
print(list(compress([1, 2], [])))
print(list(compress(count(), [0, 1, 0, 1])))

# --- pairwise ---------------------------------------------------------------------
print(list(pairwise("ABCD")))
print(list(pairwise([1])))
print(list(pairwise([])))
print(list(pairwise(range(4))))

# --- batched ----------------------------------------------------------------------
print(list(batched("ABCDEFG", 3)))
print(list(batched("ABCDEF", 2)))
print(list(batched([], 4)))
print(list(batched("ABCD", 2, strict=True)))
try:
    list(batched("ABCDE", 2, strict=True))
except ValueError as exc:
    print("ValueError", exc)
try:
    batched("AB", 0)
except ValueError as exc:
    print("ValueError", exc)

# --- groupby ----------------------------------------------------------------------
print([key for key, group in groupby("AAAABBBCCDAABBB")])
print([(key, list(group)) for key, group in groupby("AAAABBBCCD")])
print([(key, list(group)) for key, group in groupby([1, 1, 2, 3, 3], lambda x: x * 10)])
print([(key, list(group)) for key, group in groupby([], key=lambda x: x)])
groups = groupby("aabb")
key_one, group_one = next(groups)
key_two, group_two = next(groups)
print(key_one, key_two)
print(list(group_one))
print(list(group_two))

# --- traceback.py-shaped integration ----------------------------------------------
rows = list(zip_longest("abc", "ab", fillvalue=""))
print(rows)
print([(key, [pair[0] for pair in group]) for key, group in groupby(rows, key=lambda x: x[1])])
