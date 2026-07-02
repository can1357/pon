class MetaA(type):
    def __new__(cls, name, bases, namespace, **kwargs):
        return super().__new__(cls, name, bases, namespace, **kwargs)

class MetaB(MetaA):
    def __new__(cls, name, bases, namespace, **kwargs):
        return super().__new__(cls, name, bases, namespace, **kwargs)

class X(metaclass=MetaB):
    pass

print("depth2-pure-python: ok", X.__name__)

class MetaC(MetaA):
    pass

class Z(metaclass=MetaC):
    pass

print("depth2-inherited-new: ok", Z.__name__)
