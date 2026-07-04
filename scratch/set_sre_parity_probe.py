s={1,2}
f=frozenset({2,3})
print(s.symmetric_difference([2,4]), f.symmetric_difference([3,5]), type(f.symmetric_difference([3])).__name__)
s.symmetric_difference_update((5,))
print(sorted(s))
for label, receiver in [('set', {1}), ('frozenset', frozenset({1}))]:
    try:
        receiver.symmetric_difference(1)
    except TypeError as e:
        print(label, type(e).__name__, e)
    except Exception as e:
        print(label, type(e).__name__, e)
    else:
        print(label, 'NOERROR')
import re
m=re.match(r'(a)','a')
try: m.group(2)
except IndexError as e: print('IE', e)
try: m.group('x')
except IndexError as e: print('IE2', e)
print(m.group(1), m[0])
