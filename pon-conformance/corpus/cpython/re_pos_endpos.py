import re

p = re.compile(r'\d+')
m = p.match('ab12', 2)
assert m is not None
assert m.group() == '12'
assert m.span() == (2, 4)
assert m.groups() == ()

# pos changes where matching begins, but ^ still anchors to the real start.
assert re.compile(r'^b').match('ab', 1) is None

# A tokenizer-style loop advances pos through the input.
token_re = re.compile(r'\s*(?:(?P<word>[A-Za-z]+)|(?P<int>\d+)|(?P<op>[+*/-]))')
text = 'sum 12 + 34'
pos = 0
tokens = []
while pos < len(text):
    m = token_re.match(text, pos)
    assert m is not None
    assert m.end() > pos
    kind = m.lastgroup
    tokens.append((kind, m.group(kind), m.span(kind)))
    pos = m.end()
assert tokens == [
    ('word', 'sum', (0, 3)),
    ('int', '12', (4, 6)),
    ('op', '+', (7, 8)),
    ('int', '34', (9, 11)),
]
assert pos == len(text)

# endpos limits the searchable slice and $ anchors at that artificial end.
end_re = re.compile(r'\d+$')
m = end_re.search('12 34', 0, 2)
assert m is not None
assert m.group() == '12'
assert m.span() == (0, 2)
assert end_re.search('12 34', 0, 1).group() == '1'
assert end_re.search('12 34', 0, 3) is None
m = end_re.search('12 34', 3, 5)
assert m is not None
assert m.group() == '34'
assert m.span() == (3, 5)

print('ok')
