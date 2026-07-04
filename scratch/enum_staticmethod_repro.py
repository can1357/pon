from enum import Enum
d = {'default': 1, 'nofallback': 2}
class W(Enum):
    default = 1
    nofallback = 2
    def __str__(self):
        return self.name
    @staticmethod
    def from_string(mode_name):
        g = d[mode_name]
        return W(g)
print(W.from_string('default'))
print(type(W.__dict__.get('from_string')).__name__ if hasattr(W, '__dict__') else '?')
print(W.from_string)
