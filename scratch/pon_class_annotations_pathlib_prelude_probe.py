_pon_builtins = __import__('builtins')
_pon_builtins.pathlib = __import__('pathlib')
from __future__ import annotations
class License:
    file: pathlib.Path | None
print(License.__annotations__)
