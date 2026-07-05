from dataclasses import dataclass, field
@dataclass
class P:
    x: int
    y: str = 'd'
p = P(1)
print(p.x, p.y, P(2, 'z'))
