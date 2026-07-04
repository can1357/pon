import _typing
print(_typing._idfunc(9))
try:
    _typing._idfunc(1, 2)
except TypeError as e:
    print('TE ok')
