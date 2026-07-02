from re import _parser as p
t = p.parse(r'(\w+) (\w+)', 0)
print("parsed ok")
from re import _compiler as c
code = c._code(t, 0)
print("code ok", len(code))
