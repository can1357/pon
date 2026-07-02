class SlotOnly:
    __slots__ = ("x",)

    def __init__(self, value):
        self.x = value

slot = SlotOnly(7)
print(slot.x)
try:
    slot.extra = 9
except AttributeError:
    print("slot-only rejects dict")

class WithDict:
    __slots__ = ("x", "__dict__")

with_dict = WithDict()
with_dict.x = 3
with_dict.extra = 4
print(with_dict.x, with_dict.extra)

class BaseSlots:
    __slots__ = ("base",)

class ChildSlots(BaseSlots):
    __slots__ = ("child",)

child = ChildSlots()
child.base = "b"
child.child = "c"
print(child.base + child.child)

class DictChild(BaseSlots):
    pass

dict_child = DictChild()
dict_child.anything = "dict"
print(dict_child.anything)

try:
    class Left:
        __slots__ = ("left",)

    class Right:
        __slots__ = ("right",)

    class Bad(Left, Right):
        pass
except TypeError:
    print("layout conflict")
