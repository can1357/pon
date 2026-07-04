import re
class style:
    RESET = 'r'
    @staticmethod
    def strip(string):
        return re.sub(r'x+', '', string)
print(style.strip('axxb'))
print(style().strip('cxxd'))
f = style.strip
print(f('exxf'), type(f).__name__)
import functools
@functools.lru_cache()
def g():
    return style.strip('gxxh')
print(g())
