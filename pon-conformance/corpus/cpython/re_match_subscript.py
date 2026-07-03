import re

m = re.match(r'(?P<a>\d+)-(?P<b>\w+)', '12-xy')
assert m is not None
assert m[0] == m.group(0) == '12-xy'
assert m[1] == m.group(1) == '12'
assert m[2] == m.group(2) == 'xy'
assert m['a'] == m.group('a') == '12'
assert m['b'] == m.group('b') == 'xy'
assert (m[0], m[1], m[2], m['a'], m['b']) == ('12-xy', '12', 'xy', '12', 'xy')
assert m.groups() == ('12', 'xy')
assert m.span(0) == (0, 5)

optional = re.match(r'(a)(b)?', 'a')
assert optional is not None
assert optional[0] == optional.group(0) == 'a'
assert optional[1] == optional.group(1) == 'a'
assert optional[2] is None
assert optional[2] == optional.group(2)
assert optional.groups() == ('a', None)

print('ok')
