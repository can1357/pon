import os

BASE = "os_listdir_fixture"


def ignore(call, path):
    try:
        call(path)
    except OSError:
        pass


for path in [BASE + "/b.txt", BASE + "/a.txt"]:
    ignore(os.unlink, path)
for path in [BASE + "/empty", BASE]:
    ignore(os.rmdir, path)

os.makedirs(BASE + "/empty")
for path in [BASE + "/b.txt", BASE + "/a.txt"]:
    handle = open(path, "w")
    handle.write(path + "\n")
    handle.close()

class Pathish:
    def __fspath__(self):
        return BASE

print("LISTDIR path", ",".join(sorted(os.listdir(BASE))))
print("LISTDIR pathlike", ",".join(sorted(os.listdir(Pathish()))))
print("LISTDIR none has fixture", BASE in os.listdir(None))
print("LISTDIR default has fixture", BASE in os.listdir())
