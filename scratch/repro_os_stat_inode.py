import os
s = os.stat('/tmp')
print(s.st_ino > 0)
print(s.st_dev > 0)
