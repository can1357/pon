import re
m=re.match(r'(a)','a')
try: m.group(2)
except IndexError as e: print('IE', e)
try: m.group('x')
except IndexError as e: print('IE2', e)
print(m.group(1), m[0])
