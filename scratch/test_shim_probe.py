import _testsinglephase
import _testinternalcapi
print(_testsinglephase.__name__)
print(_testsinglephase.int_const, _testsinglephase.str_const)
print(_testsinglephase.initialized_count())
class Plain:
    pass
print(_testinternalcapi.has_inline_values(Plain()))
print(_testinternalcapi.has_inline_values(1))
