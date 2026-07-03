# decimal should expose a coherent public surface whether CPython serves the
# C accelerator or the pure-Python fallback. pon currently exercises the
# fallback path, so this catches the post-import `sys.modules['decimal']`
# adoption that makes `from decimal import Decimal` see the re-exported API.
import sys
import decimal
from decimal import Decimal, getcontext

print(decimal.__name__)
print(decimal is sys.modules['decimal'])
print(decimal.Decimal is Decimal)
print(decimal.getcontext is getcontext)
print(Decimal('1.25') + Decimal('2.75'))
print(type(getcontext()).__name__)
print(Decimal.__module__)
