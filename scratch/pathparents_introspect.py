import pathlib
from pathlib import PurePath
print('pathlib-file', getattr(pathlib, '__file__', None))
print('module', PurePath.parents.fget.__module__)
print('globals-name', PurePath.parents.fget.__globals__.get('__name__'))
print('globals-file', PurePath.parents.fget.__globals__.get('__file__'))
print('has-global', '_PathParents' in PurePath.parents.fget.__globals__)
print('pathlib-has', hasattr(pathlib, '_PathParents'))
