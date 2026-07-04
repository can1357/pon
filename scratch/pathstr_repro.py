import pathlib
p = pathlib.Path("/tmp").absolute()
print("absolute ok, now str()")
print("str:", str(p))
