import importlib._bootstrap as b
import importlib._bootstrap_external as be
print(b.__name__)
print(hasattr(b, 'BuiltinImporter'))
print(hasattr(b, 'FrozenImporter'))
print(be.__name__)
for name in ['PathFinder', 'WindowsRegistryFinder', 'FileFinder', 'NamespaceLoader', 'SourceFileLoader', 'SourcelessFileLoader', 'ExtensionFileLoader', 'AppleFrameworkLoader']:
    print(name, hasattr(be, name))
