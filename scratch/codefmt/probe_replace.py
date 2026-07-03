def gen():
    yield 1

co = gen.__code__
print("T1", type(co).__name__)
co2 = co.replace()
print("T2", type(co2).__name__, co2 is co)
print("T3", co2.co_name, co2.co_flags == co.co_flags)
co3 = co.replace(co_flags=co.co_flags | 0x100)
print("T4", co3.co_flags == (co.co_flags | 0x100), co.co_flags & 0x100)
co4 = co3.replace(co_filename='cleaned.py')
print("T5", co4.co_filename, co4.co_flags == co3.co_flags)
gen.__code__ = co3
print("T6", gen.__code__.co_flags == co3.co_flags)
try:
    co.replace(bogus=1)
except TypeError as e:
    print("T7", "unexpected keyword" in str(e))
try:
    co.replace(co_flags='x')
except TypeError as e:
    print("T8 TypeError")
try:
    gen.__code__ = 5
except TypeError as e:
    print("T9", e)
try:
    co.replace(1)
except TypeError as e:
    print("T10", e)
