import os

BASE = "os_walk_fixture"


def ignore(call, path):
    try:
        call(path)
    except OSError:
        pass


# Clean a previous fixture without relying on os.walk.
for path in [
    BASE + "/keep/deep/deep.txt",
    BASE + "/keep/keep.txt",
    BASE + "/skip/child/hidden.txt",
    BASE + "/skip/skip.txt",
    BASE + "/root.txt",
]:
    ignore(os.unlink, path)
for path in [
    BASE + "/keep/deep",
    BASE + "/keep",
    BASE + "/skip/child",
    BASE + "/skip",
    BASE + "/empty",
    BASE,
]:
    ignore(os.rmdir, path)

os.makedirs(BASE + "/keep/deep")
os.makedirs(BASE + "/skip/child")
os.makedirs(BASE + "/empty")


def touch(path):
    handle = open(path, "w")
    handle.write(path + "\n")
    handle.close()


touch(BASE + "/root.txt")
touch(BASE + "/keep/keep.txt")
touch(BASE + "/keep/deep/deep.txt")
touch(BASE + "/skip/skip.txt")
touch(BASE + "/skip/child/hidden.txt")


def normalized(rows):
    out = []
    for root, dirs, files in rows:
        out.append((root, ",".join(sorted(dirs)), ",".join(sorted(files))))
    return sorted(out)


def print_rows(label, rows):
    print(label)
    for root, dirs, files in normalized(rows):
        print(root + " | D=" + dirs + " | F=" + files)


print_rows("BASIC", list(os.walk(BASE)))

pruned = []
for root, dirs, files in os.walk(BASE):
    if root == BASE:
        dirs.sort()
        dirs.remove("skip")
    pruned.append((root, list(dirs), list(files)))

for root, dirs, files in pruned:
    assert root != BASE + "/skip"
    assert not root.startswith(BASE + "/skip/")
print_rows("PRUNE", pruned)

bottom = list(os.walk(BASE, topdown=False))
position = {}
for index, row in enumerate(bottom):
    position[row[0]] = index
assert position[BASE + "/keep/deep"] < position[BASE + "/keep"] < position[BASE]
assert position[BASE + "/skip/child"] < position[BASE + "/skip"] < position[BASE]
assert position[BASE + "/empty"] < position[BASE]
print("BOTTOMUP_INVARIANTS children-before-parents OK")
print_rows("BOTTOMUP", bottom)
