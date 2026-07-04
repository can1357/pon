class A:
    def __init__(self):
        super().__init__()
        print("after super")

try:
    A()
    print("ok")
except BaseException as exc:
    print("err", type(exc).__name__, str(exc))
