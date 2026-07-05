# The very first str method attribute load rides a native-origin string
# through a chained subscript receiver (meson python_info.py's
# `sys.path[0].endswith('scripts')` shape) — must work with no prior
# string-method warm-up anywhere in the program.
import sys

if sys.path[0].endswith('definitely-not-a-suffix'):
    print('unexpected')
else:
    print('cold chained load ok')
print(sys.path[0].endswith('definitely-not-a-suffix'))

import os

print(os.getcwd().endswith('\0'))

# Missing attributes on builtin receivers raise AttributeError; hasattr and
# three-arg getattr depend on the exception kind.
print(hasattr('abc', 'not_a_method'))
print(getattr('abc', 'not_a_method', 'fallback'))
try:
    'abc'.not_a_method
except AttributeError:
    print('AttributeError')
