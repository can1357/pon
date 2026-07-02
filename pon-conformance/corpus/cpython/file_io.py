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
    seen = 0
    beta_ok = False
    omega_ok = False
    for line in lines:
        if seen == 1:
            beta_ok = line == "beta\n"
        if seen == 4:
            omega_ok = line == "omega"
        seen += 1
    print(seen, beta_ok, omega_ok)

count = 0
for line in open(path, "r"):
    count += 1
print(count)
