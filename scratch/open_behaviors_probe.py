import os

path = "scratch/open_probe_output.txt"
opener_target = "scratch/open_probe_via_opener.txt"

open(path, "wb", buffering=0).write(b"raw")
fd = os.open(path, os.O_WRONLY | os.O_APPEND, 0o644)
f = open(fd, "w", closefd=False)
f.write("X")
f.close()
os.write(fd, b"Y")
os.close(fd)

g = open("ignored.txt", "w", opener=lambda p, flags: os.open(opener_target, flags, 0o644))
g.write("via-opener")
g.close()

data = open(b"scratch/open_probe_input.txt", "rb").read()
print(open(path, "rb").read())
print(open(opener_target).read())
print(data)
