import re

# Possessive optional grouped repeat keeps the successful digits.
m = re.fullmatch(r'(?:[0-9]+)?+', '12')
assert m is not None
assert m.group() == '12'
assert m.groups() == ()
assert m.span() == (0, 2)

# Alternation order interacts with possessive and atomic repeats: once "a" wins,
# the engine must not backtrack to "ab".
assert re.fullmatch(r'(?:a|ab)*+', 'abab') is None
m = re.match(r'(?:a|ab)*', 'abab')
assert m is not None
assert m.group() == 'a'
assert m.span() == (0, 1)
assert re.fullmatch(r'(?:a|ab)*', 'abab').group() == 'abab'
assert re.fullmatch(r'(?>(?:a|ab)*)', 'abab') is None

# Zero-width repeats are still valid matches and report the empty span.
m = re.fullmatch(r'(?:a*)+', '')
assert m is not None
assert m.group() == ''
assert m.span() == (0, 0)

# x*+ is equivalent to (?>x*) for both success and backtracking prevention.
for pattern in (r'a*+', r'(?>a*)'):
    m = re.fullmatch(pattern, 'aaa')
    assert m is not None
    assert m.group() == 'aaa'
    assert m.groups() == ()
    assert m.span() == (0, 3)
assert re.fullmatch(r'a*+a', 'aaa') is None
assert re.fullmatch(r'(?>a*)a', 'aaa') is None

# Search also sees possessive repeats when no backtracking is required.
m = re.search(r'a++b', 'xaaabz')
assert m is not None
assert m.group() == 'aaab'
assert m.span() == (1, 5)

# A reduced packaging.version-style pattern uses a possessive release repeat.
version_re = re.compile(
    r'''
    v?
    (?:(?P<epoch>[0-9]+)!)?
    (?P<release>[0-9]+(?:\.[0-9]+)*+)
    (?P<dev>[-_.]?(?P<dev_l>dev)[-_.]?(?P<dev_n>[0-9]+)?)?
    (?:\+(?P<local>[a-z0-9]+(?:[-_.][a-z0-9]+)*+))?
    ''',
    re.VERBOSE | re.IGNORECASE,
)

m = version_re.fullmatch('0.dev0')
assert m is not None
assert m.group('release') == '0'
assert m.group('dev') == '.dev0'
assert m.group('dev_l') == 'dev'
assert m.group('dev_n') == '0'
assert m.groups() == (None, '0', '.dev0', 'dev', '0', None)
assert m.groupdict() == {
    'epoch': None,
    'release': '0',
    'dev': '.dev0',
    'dev_l': 'dev',
    'dev_n': '0',
    'local': None,
}
assert m.span('release') == (0, 1)

m = version_re.fullmatch('1.2.3.dev4')
assert m is not None
assert m.group('release') == '1.2.3'
assert m.group('dev_n') == '4'
assert m.groups() == (None, '1.2.3', '.dev4', 'dev', '4', None)
assert m.groupdict() == {
    'epoch': None,
    'release': '1.2.3',
    'dev': '.dev4',
    'dev_l': 'dev',
    'dev_n': '4',
    'local': None,
}
assert m.span('dev') == (5, 10)

print('ok')
