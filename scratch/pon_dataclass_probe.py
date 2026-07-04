import dataclasses
@dataclasses.dataclass(frozen=True)
class P:
    x: int
print(P(1))
