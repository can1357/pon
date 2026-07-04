class S(str):
    pass

k = S("type")
d = {k: 1}
print(type(k).__name__, k == "type", hash(k) == hash("type"), d.get("type"), d.get(k), "type" in d, k in d)
