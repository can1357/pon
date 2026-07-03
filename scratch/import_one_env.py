import importlib
import os
name = os.environ['MODULE']
importlib.import_module(name)
print('ok', name)
