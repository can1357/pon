from typing import NamedTuple, TypedDict
A = NamedTuple.__mro_entries__((NamedTuple,))[0]
B = TypedDict.__mro_entries__((TypedDict,))[0]
print(type(A).__name__, A.__name__ if hasattr(A, "__name__") else None)
print(type(B).__name__, B.__name__ if hasattr(B, "__name__") else None)
print(A, B)
