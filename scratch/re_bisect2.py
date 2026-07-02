from re import _compiler as c
pat = c.compile('a+', 0)
print("compiler.compile ok", pat)
import re
pat2 = re.compile('a+')
print("re.compile ok", pat2)
m = re.match(r'(\w+) (\w+)', 'Isaac Newton')
print(m)
