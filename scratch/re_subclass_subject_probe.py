import re
class S(str): pass
print(re.sub(r'l+', 'L', S('hello')))
print(re.match(r'he', S('hello')).group(0))
print(re.findall(r'o', S('foo boo')))
print(re.split(r'\s+', S('a b  c')))
