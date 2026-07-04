import os
import shutil

BASE = "rmtree_probe_dir"
try:
    os.unlink(BASE + "/a/file.txt")
except OSError:
    pass
try:
    os.rmdir(BASE + "/a")
except OSError:
    pass
try:
    os.rmdir(BASE)
except OSError:
    pass
os.makedirs(BASE + "/a")
f = open(BASE + "/a/file.txt", "w")
f.write("x")
f.close()
shutil.rmtree(BASE)
print("removed")
