import importlib
import sys

cluster = sys.argv[1:]
for name in cluster:
    importlib.import_module(name)
    print('ok', name)
