from ppkg import PurePath
from ppkg import b

print('top', PurePath('top').parents)
print('direct', b.direct())
print('nested', b.nested())
print('comp', b.comp())
print('via_func', b.via_func())
