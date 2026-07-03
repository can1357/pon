# GC visibility for leaked-box native iterators: every registered family must
# survive gc.collect() between construction and (further) consumption, because
# the holder objects live outside the GC heap and pin their sources only
# through the per-family root registries (native/itertools.rs,
# types/lazy_iter.rs, native/builtins_mod.rs, abi/str_.rs).
import gc
import itertools

# chain: the original residual (held source freed by collect).
it = itertools.chain([1, 2], [3])
gc.collect()
print(list(it))

# chain.from_iterable with a collect mid-iteration.
it = itertools.chain.from_iterable([[1], [2, 3]])
print(next(it))
gc.collect()
print(list(it))

# cycle: source iterator plus the saved-items vec.
c = itertools.cycle([7, 8])
print(next(c))
gc.collect()
print(next(c), next(c), next(c))

# count / repeat: boxed current/step and repeated-object holders.
k = itertools.count(10, 2)
gc.collect()
print(next(k), next(k))
r = itertools.repeat([9], 3)
gc.collect()
print(list(r))

s = itertools.islice([10, 20, 30, 40], 1, 3)
gc.collect()
print(list(s))

sm = itertools.starmap(pow, [(2, 3), (3, 2)])
gc.collect()
print(list(sm))

zl = itertools.zip_longest([1, 2, 3], "ab", fillvalue="-")
gc.collect()
print(list(zl))

p = itertools.product([1, 2], "ab")
gc.collect()
print(list(p))

pm = itertools.permutations("ab", 2)
gc.collect()
print(list(pm))

cb = itertools.combinations([1, 2, 3], 2)
gc.collect()
print(list(cb))

ac = itertools.accumulate([1, 2, 3, 4])
gc.collect()
print(list(ac))

ff = itertools.filterfalse(lambda x: x % 2, [1, 2, 3, 4])
gc.collect()
print(list(ff))

tw = itertools.takewhile(lambda x: x < 3, [1, 2, 3, 1])
dw = itertools.dropwhile(lambda x: x < 3, [1, 2, 3, 1])
gc.collect()
print(list(tw), list(dw))

cp = itertools.compress("abcd", [1, 0, 1, 0])
gc.collect()
print(list(cp))

pw = itertools.pairwise([1, 2, 3])
print(next(pw))
gc.collect()
print(list(pw))

bt = itertools.batched("abcde", 2)
gc.collect()
print(list(bt))

# groupby: the shared cursor and each _grouper must stay rooted mid-walk.
gb = itertools.groupby("aabbc")
gc.collect()
out = []
for key_char, group in gb:
    gc.collect()
    out.append((key_char, list(group)))
print(out)

# Builtins: lazy-iterator boxes and native payload holders.
e = enumerate(["a", "b", "c"])
gc.collect()
print(list(e))

m = map(lambda x: x + 1, [1, 2])  # the lambda's only reference is the map box
gc.collect()
print(list(m))

f = filter(None, [0, 1, "", "x"])
gc.collect()
print(list(f))

z = zip([1, 2], "ab")
gc.collect()
print(list(z))

rv = reversed([1, 2, 3])
gc.collect()
print(list(rv))

# str iterator borrows its GC-heap unicode receiver.
si = iter("héllo")
print(next(si))
gc.collect()
print(list(si))


# Legacy __getitem__ sequence iterator.
class Seq:
    def __getitem__(self, index):
        if index > 2:
            raise IndexError
        return index * 11


sq = iter(Seq())
gc.collect()
print(list(sq))

# iter(callable, sentinel): the callable is a bound method whose receiver is
# reachable only by piercing the method box.
vals = [1, 2, 9]
ci = iter(vals.pop, 9)
gc.collect()
print(list(ci), vals)

# collect() inside the consuming loop: every step must stay rooted.
total = 0
for value in itertools.chain([1, 2], map(lambda x: x * 10, [3, 4])):
    gc.collect()
    total += value
print(total)

# t-string templates: the leaked Template/Interpolation boxes hold GC tuples
# and strings (abi/format.rs registry).
who = "wo" + "rld"
t = t"hello {who}!"
gc.collect()
print(t.strings, t.interpolations[0].value)
combined = t"a {who}" + t"b"
gc.collect()
print(combined.strings, combined.values)
