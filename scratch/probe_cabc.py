import _collections_abc as m
print('mro', m.MutableMapping.__mro__)
print('mapping-own-ne', '__ne__' in dict(m.Mapping.__dict__))
print('mm-update', m.MutableMapping.update)
print('mm-ne', m.MutableMapping.__ne__)
