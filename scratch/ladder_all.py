import re

p1 = re.compile('a|b')
print('L1', p1.match('a'), p1.match('b'), p1.match('c'))
p2 = re.compile('[a-z]+')
print('L2', p2.match('hello world'), p2.findall('ab cd'))
p3 = re.compile(r'\w+')
print('L3', p3.match('abc_123 x'), p3.findall('foo bar'))
p4 = re.compile(r'\d')
print('L4', p4.match('7'), p4.search('ab3cd'), p4.match('x'))
p5 = re.compile('(?i)abc')
print('L5', p5.match('ABC'), p5.match('aBc'), p5.match('xbc'))
