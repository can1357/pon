import sys, tarfile, shutil
print('step tarfile filter')
tarfile.TarFile.extraction_filter = staticmethod(tarfile.fully_trusted_filter)
print('ok tarfile')
print('step which')
PATCH = shutil.which('patch')
print('ok which', bool(PATCH))
from functools import lru_cache
@lru_cache(maxsize=None)
def f(x): return x*2
print('ok lru', f(3))
from dataclasses import dataclass
import typing as T
@dataclass(eq=False)
class PackageDefinition:
    name: str
    subprojects_dir: str = 'x'
print('ok dataclass', PackageDefinition('a').name)
