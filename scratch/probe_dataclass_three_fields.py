from dataclasses import dataclass

@dataclass
class SharedFunctionDecl:
    name: str
    ret: str
    params: str

print(SharedFunctionDecl('a', 'b', 'c'))
