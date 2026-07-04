samples = [
    "ascii",
    "é",
    "😀",
    "mixé😀\n",
    ["é", "😀", "ascii"],
    {"é": "😀", "ascii": ["é", 1]},
]

for value in samples:
    print(ascii(value))

import builtins
print(builtins.ascii("é"))
