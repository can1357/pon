import importlib
for name in ('typing', 're', 'importlib', 'importlib.metadata'):
    mod = importlib.import_module(name)
    print(name, getattr(mod, '__file__', None))
import typing
print('typing.Match', typing.Match.__module__, typing.Match)
