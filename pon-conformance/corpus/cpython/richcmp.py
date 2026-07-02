print("rich comparisons")


log = []


class LeftOrder:
    def __lt__(self, other):
        log.append("LeftOrder.__lt__")
        return NotImplemented


class RightOrder:
    def __gt__(self, other):
        log.append("RightOrder.__gt__")
        return True


print("lt reflected", LeftOrder() < RightOrder(), log)


log = []


class LeftEq:
    def __eq__(self, other):
        log.append("LeftEq.__eq__")
        return NotImplemented


class RightEq:
    def __eq__(self, other):
        log.append("RightEq.__eq__")
        return True


print("eq reflected", LeftEq() == RightEq(), log)


log = []


class MissingCompare:
    def __init__(self, label):
        self.label = label

    def __eq__(self, other):
        log.append(self.label + ".__eq__")
        return NotImplemented

    def __ne__(self, other):
        log.append(self.label + ".__ne__")
        return NotImplemented


left = MissingCompare("left")
right = MissingCompare("right")
print("eq different", left == right)
print("ne different", left != right)
print("eq identical", left == left)
print("ne identical", left != left)


log = []


class NoLess:
    def __lt__(self, other):
        log.append("NoLess.__lt__")
        return NotImplemented


class NoGreater:
    def __gt__(self, other):
        log.append("NoGreater.__gt__")
        return NotImplemented


try:
    NoLess() < NoGreater()
except TypeError:
    print("ordered TypeError", log)


log = []


class Base:
    def __lt__(self, other):
        log.append("Base.__lt__")
        return False


class Sub(Base):
    def __gt__(self, other):
        log.append("Sub.__gt__")
        return True


print("subclass reflected first", Base() < Sub(), log)
