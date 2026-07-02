for c in "abc":
    print("iter", c)
print("contain", "a" in "abc")
import re
print("escape", re.escape("a+b"))
p = re.compile('a+')
print("compiled", p)
