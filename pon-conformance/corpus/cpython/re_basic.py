import re

# --- match / search / fullmatch ---
m = re.match(r'(\w+) (\w+)', 'Isaac Newton, physicist')
print("match group0", m.group(0))
print("match groups12", m.group(1), m.group(2))
print("match group tuple", m.group(1, 2))
print("match groups", m.groups())
print("match span", m.span(), m.start(), m.end())
print("match span2", m.span(2), m.start(2), m.end(2))
print("match miss", re.match(r'world', 'hello world'))
print("search span", re.search(r'world', 'hello world').span())
print("search miss", re.search(r'zebra', 'hello world'))
print("fullmatch hit", re.fullmatch(r'p.*n', 'python').group(0))
print("fullmatch miss", re.fullmatch(r'p.*n', 'pythonic'))
print("truthiness", bool(re.match(r'a', 'abc')), bool(re.search(r'q', 'abc')))

# --- named groups ---
m = re.match(r'(?P<first>\w+) (?P<last>\w+)', 'Malcolm Reynolds')
print("named groups", m.group('first'), m.group('last'))
print("named groupdict", m.groupdict())
print("named last", m.lastindex, m.lastgroup)

# --- optional group unmatched ---
m = re.match(r'(a)(b)?', 'a')
print("optional groups", m.groups())
print("optional default", m.groups('missing'))
print("optional group2", m.group(2))
print("optional span2", m.span(2), m.start(2), m.end(2))

# --- findall ---
print("findall digits", re.findall(r'\d+', 'a1 b22 c333'))
print("findall pairs", re.findall(r'(\w+)=(\d+)', 'x=1, y=22'))
print("findall optional", re.findall(r'(a)(b)?', 'ab a'))
print("findall empty", re.findall(r'x*', 'axbx'))

# --- finditer ---
for m in re.finditer(r'\d+', 'a1 b22 c333'):
    print("finditer", m.span(), m.group(0))

# --- split ---
print("split", re.split(r'\W+', 'Words, words, words.'))
print("split capture", re.split(r'(\W+)', 'Words, words, words.'))
print("split maxsplit", re.split(r'\W+', 'Words, words, words.', maxsplit=1))
print("split alt groups", re.split(r'(x)|(y)', 'axbyc'))
print("split empty", re.split(r'x*', 'abc'))

# --- sub / subn ---
print("sub", re.sub(r'\d+', '#', 'a1 b22 c333'))
print("sub backref", re.sub(r'(\w+) (\w+)', r'\2 \1', 'first second'))
print("sub named ref", re.sub(r'(?P<word>ab)', r'<\g<word>>', 'ab cd ab'))
print("sub empty", re.sub(r'x*', '-', 'abc'))
print("sub count", re.sub(r'\d+', '#', 'a1 b2 c3', count=2))
print("subn", re.subn(r'\d+', '#', 'a1 b2 c3'))
print("sub escape repl", re.sub(r'\d', r'\\n', 'a1'))


def shout(m):
    return m.group(0).upper()


print("sub callable", re.sub(r'\w+', shout, 'ab cd'))

# --- flags: I / M / S ---
print("flag I findall", re.findall(r'[a-z]+', 'AbC dEf', re.I))
print("flag I match", bool(re.match(r'abc', 'ABC', re.IGNORECASE)))
print("flag M findall", re.findall(r'^\w+', 'one two\nthree four', re.M))
print("flag M anchors", re.findall(r'\w+$', 'one two\nthree four', re.MULTILINE))
print("flag S dot", bool(re.match(r'a.c', 'a\nc', re.S)), bool(re.match(r'a.c', 'a\nc')))
print("flag S findall", re.findall(r'a.c', 'a\nc abc', re.DOTALL))
print("flags combined", re.findall(r'^[a-z]+', 'One\nTWO\nthree', re.I | re.M))

# --- escapes ---
print("escape", re.escape('1+1=2 (really?)'))
print("escaped dot", bool(re.match(r'\d\.\d', '1.5')), bool(re.match(r'\d\.\d', '1x5')))
print("word boundary", re.findall(r'\bword\b', 'word sword wordy word'))
print("whitespace", re.search(r'\s+', 'a \t b').span())
print("escaped meta", re.findall(r'\$\d+', 'costs $5 or $10'))
print("class escape", re.findall(r'[\d.]+', 'v1.2 and 3.4.5'))

# --- compiled pattern objects ---
p = re.compile(r'(ab)+')
print("pattern attrs", p.pattern, p.groups)
print("pattern match", bool(p.match('abab')), p.match('abab').group(0))
print("pattern search", p.search('xxabab').span())
print("pattern findall", p.findall('ab abab'))
print("pattern flags I", bool(re.compile(r'abc', re.I).match('AbC')))
