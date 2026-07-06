f = open("target/pon_file_io_probe2.txt", "w")
w = f.write
wl = f.writelines
print("write attr:", w)
print("writelines attr:", wl)
print(w("abc"))
try:
    wl(["x"])
    print("wl ok")
except NotImplementedError as e:
    print("wl stub:", e)
g = getattr(f, "writelines")
try:
    g(["y"])
    print("getattr wl ok")
except NotImplementedError as e:
    print("getattr wl stub:", e)
f.close()
