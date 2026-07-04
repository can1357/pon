import os
_orig_stat = os.stat
class Proxy:
    def __init__(self, value):
        self._value = value
        self.st_ino = getattr(value, 'st_ino', 0)
        self.st_dev = getattr(value, 'st_dev', 0)
    def __getattr__(self, name):
        return getattr(self._value, name)
    def __getitem__(self, index):
        return self._value[index]
    def __len__(self):
        return len(self._value)
    def __iter__(self):
        return iter(self._value)
def stat(*args, **kwargs):
    return Proxy(_orig_stat(*args, **kwargs))
os.stat = stat
from pathlib import Path
print([str(p) for p in Path('/tmp/a/b').parents][:3])
