import os
import pathlib
BASE = 'empty_iterdir_probe_dir'
try:
    os.rmdir(BASE)
except OSError:
    pass
os.mkdir(BASE)
print('before')
print(any(pathlib.Path(BASE).iterdir()))
print('after')
