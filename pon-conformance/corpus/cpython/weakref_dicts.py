import gc
import weakref


class Obj:
    def __init__(self, tag):
        self.tag = tag

    def __repr__(self):
        return f"Obj({self.tag})"


# WeakKeyDictionary: unittest.signals instantiates one at import.
wkd = weakref.WeakKeyDictionary()
k1, k2 = Obj("k1"), Obj("k2")
wkd[k1] = 1
wkd[k2] = 2
print("wkd len", len(wkd))
print("wkd lookups", wkd[k1], wkd.get(k2), wkd.get(Obj("zz"), "dflt"))
print("wkd contains", k1 in wkd)
del wkd[k2]
print("wkd len after del", len(wkd))
print("wkd contains dead key", k2 in wkd)

# Key death drops the entry once the collector runs.
del k2
gc.collect()
gc.collect()
print("wkd len after key death", len(wkd))

# WeakValueDictionary: logging keeps its named-handler map in one.
wvd = weakref.WeakValueDictionary()
v1, v2 = Obj("v1"), Obj("v2")
wvd["a"] = v1
wvd["b"] = v2
print("wvd len", len(wvd))
print("wvd lookups", wvd["a"], wvd.get("b"), wvd.get("zz", "dflt"))
print("wvd contains", "a" in wvd)
del wvd["b"]
print("wvd len after del", len(wvd))
print("wvd contains removed", "b" in wvd)
print("wvd pop", wvd.pop("a").tag, wvd.pop("gone", "pdflt"))
print("wvd len after pop", len(wvd))

# Value death drops the entry once the collector runs.
wvd["c"] = v2
del v2
gc.collect()
gc.collect()
print("wvd len after value death", len(wvd))
