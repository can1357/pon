class C:
    __slots__ = ('x', '_str')
c = C()
try:
    v = c._str
    print("no error, got", v)
except AttributeError as e:
    print("caught AttributeError:", e)
except BaseException as e:
    print("caught other:", type(e).__name__, e)
print("after")
# lazy-cache pattern like pathlib.__str__
class P:
    __slots__ = ('_str',)
    def __str__(self):
        try:
            return self._str
        except AttributeError:
            self._str = "computed"
            return self._str
print("str(P()):", str(P()))
