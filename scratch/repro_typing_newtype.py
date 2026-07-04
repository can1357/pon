from typing import NewType
T = NewType('T', str)
print(T('ok'))
print(T.__name__)
print(T.__supertype__ is str)
