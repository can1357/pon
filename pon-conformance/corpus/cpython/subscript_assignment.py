# Subscript assignment protocol: user __setitem__/__delitem__ on plain
# classes, dict subclass delegation via super(), sys.modules as a live dict
# (the collections/__init__.py registration shape), and the TypeError legs
# for receivers without item assignment/deletion support.


# --- sys.modules is a real dict: the collections/__init__.py line-32 shape
import sys

class Payload:
    pass

payload = Payload()
payload.tag = "registered"
sys.modules["subscript_assignment_probe"] = payload
print(type(sys.modules) is dict)
print("subscript_assignment_probe" in sys.modules)
print(sys.modules["subscript_assignment_probe"].tag)
print(sys.modules.get("subscript_assignment_probe") is payload)
import subscript_assignment_probe
print(subscript_assignment_probe is payload)
del sys.modules["subscript_assignment_probe"]
print("subscript_assignment_probe" in sys.modules)

# --- obj[k] = v / del obj[k] via user __setitem__/__delitem__ on a plain class
class Board:
    def __init__(self):
        self.cells = {}

    def __setitem__(self, key, value):
        self.cells[key] = value

    def __getitem__(self, key):
        return self.cells[key]

    def __delitem__(self, key):
        del self.cells[key]


board = Board()
board["corner"] = "X"
board[(1, 2)] = "O"
print(board["corner"], board[(1, 2)])
print(board.cells)
del board["corner"]
print(board.cells)

# --- dict subclass delegation via super().__setitem__ / super().__delitem__
class UpperDict(dict):
    def __setitem__(self, key, value):
        super().__setitem__(key, value.upper() if isinstance(value, str) else value)

    def __delitem__(self, key):
        super().__delitem__(key.lower())


ud = UpperDict()
ud["a"] = "quiet"
ud["b"] = 7
print(ud["a"], ud["b"], len(ud))
del ud["A".lower()]
print(dict(ud))

# --- grandchild delegation: two super() hops end in the native dict slot
class ShoutyDict(UpperDict):
    def __setitem__(self, key, value):
        super().__setitem__(key, value)


sd = ShoutyDict()
sd["q"] = "soft"
print(sd["q"], dict(sd))

# --- error leg: missing __setitem__ / __delitem__ raises TypeError
class ReadOnly:
    def __getitem__(self, key):
        return key


ro = ReadOnly()
print(ro["passthrough"])
try:
    ro["k"] = 1
except TypeError as exc:
    print("TypeError:", exc)
try:
    ro[0] = 1
except TypeError as exc:
    print("TypeError:", exc)
try:
    del ro["k"]
except TypeError as exc:
    print("TypeError:", exc)
try:
    del ro[0]
except TypeError as exc:
    print("TypeError:", exc)
try:
    object()[0] = 1
except TypeError as exc:
    print("TypeError:", exc)
