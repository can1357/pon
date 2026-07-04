import os, subprocess
print('fsdecode', repr(os.fsdecode), type(os.fsdecode))
print(os.fsdecode('/x'))
print(subprocess.list2cmdline(['/bin/echo', 'ok']))
