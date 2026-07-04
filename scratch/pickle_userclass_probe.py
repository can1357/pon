import pickle
from enum import Enum


class Plain:
    def __init__(self, name):
        self.name = name
        self.payload = {"numbers": [1, 2, 3], "tuple": ("x", {"inner": "y"})}
        self.flag = True


class Stateful:
    def __init__(self, value):
        self.value = value
        self.ignored = "not serialized"

    def __getstate__(self):
        return {"value": self.value, "restored": "from_getstate"}

    def __setstate__(self, state):
        self.value = state["value"] + 1
        self.restored = state["restored"] + "_set"


class Reduced:
    def __init__(self, value):
        self.value = value

    def __reduce__(self):
        return (Reduced, (self.value + 10,))


class Color(Enum):
    RED = "red"
    BLUE = "blue"


plain = Plain("plain")
shared = Plain("shared")
graph = {
    "plain": plain,
    "stateful": Stateful(4),
    "reduced": Reduced(5),
    "enum": Color.BLUE,
    "nested": [plain, Stateful(1), (Reduced(2), Color.RED)],
    "shared": [shared, shared],
}

roundtrip = pickle.loads(pickle.dumps(graph, protocol=pickle.HIGHEST_PROTOCOL))

print("plain", type(roundtrip["plain"]).__name__, roundtrip["plain"].name,
      roundtrip["plain"].payload["numbers"],
      roundtrip["plain"].payload["tuple"][0],
      roundtrip["plain"].payload["tuple"][1]["inner"],
      roundtrip["plain"].flag)
print("stateful", type(roundtrip["stateful"]).__name__,
      roundtrip["stateful"].value,
      roundtrip["stateful"].restored,
      hasattr(roundtrip["stateful"], "ignored"))
print("reduced", type(roundtrip["reduced"]).__name__, roundtrip["reduced"].value)
print("enum", roundtrip["enum"] is Color.BLUE, roundtrip["enum"].name, roundtrip["enum"].value)
print("nested", roundtrip["nested"][0] is roundtrip["plain"],
      roundtrip["nested"][1].value,
      roundtrip["nested"][1].restored,
      roundtrip["nested"][2][0].value,
      roundtrip["nested"][2][1] is Color.RED)
print("shared_identity", roundtrip["shared"][0] is roundtrip["shared"][1],
      roundtrip["shared"][0].name,
      roundtrip["shared"][0] is shared)
print("graph_types", [type(roundtrip[key]).__name__ for key in sorted(roundtrip)])
