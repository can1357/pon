import sys, platform
print(sys.maxunicode)
print(platform.python_version())
print(type(sys.path_importer_cache).__name__)
sys.path_importer_cache['x'] = None
print('x' in sys.path_importer_cache)
