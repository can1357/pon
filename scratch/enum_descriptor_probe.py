from enum import Enum

lookup = {"default": 1, "nofallback": 2}


class W(Enum):
    default = 1
    nofallback = 2

    def __str__(self):
        return self.name

    @staticmethod
    def from_string(mode_name):
        return W(lookup[mode_name])

    @classmethod
    def pick(cls):
        return cls.default

    def label(self):
        return "label:" + self.name

    @property
    def prop(self):
        return "prop:" + self.name


print("class staticmethod call:", W.from_string("default"))
print("class staticmethod attr type:", type(W.from_string).__name__)
print("member staticmethod call:", W.default.from_string("nofallback"))
print("member staticmethod attr type:", type(W.default.from_string).__name__)
print("class classmethod call:", W.pick())
print("class classmethod attr type:", type(W.pick).__name__)
print("member classmethod call:", W.default.pick())
print("member classmethod attr type:", type(W.default.pick).__name__)
print("member plain method call:", W.default.label())
print("member plain method attr type:", type(W.default.label).__name__)
print("class property carrier type:", type(W.prop).__name__)
print("member property value:", W.default.prop)
print("dict from_string carrier type:", type(W.__dict__["from_string"]).__name__)
print("dict pick carrier type:", type(W.__dict__["pick"]).__name__)
print("dict label carrier type:", type(W.__dict__["label"]).__name__)
print("dict prop carrier type:", type(W.__dict__["prop"]).__name__)
