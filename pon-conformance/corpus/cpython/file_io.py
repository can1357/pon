path = "target/pon_file_io_corpus.txt"

f = open(path, "w")
print(f.write("alpha\nbeta\r\ngamma\rdelta"))
f.writelines(["\n", "omega"])
f.flush()
print(f.closed)
f.close()
print(f.closed)

with open(path, "r", -1, "utf-8", None, None) as f:
    first = f.readline()
    print(first == "alpha\n", f.tell())
    f.seek(0)
    text = f.read()
    print(text == "alpha\nbeta\ngamma\ndelta\nomega")

with open(path, "r") as f:
    lines = f.readlines()
    print(len(lines), lines[1] == "beta\n", lines[4] == "omega")

count = 0
for line in open(path, "r"):
    count += 1
print(count)
