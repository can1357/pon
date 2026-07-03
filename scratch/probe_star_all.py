import asyncio

# The exact regression: the package attr must be the sibling submodule, not
# the top-level subprocess module leaked through a star-copy.
print(asyncio.subprocess.__name__)
print(type(asyncio.__all__).__name__)
print("run" in asyncio.__all__, "create_subprocess_exec" in asyncio.__all__)
print("Popen" in asyncio.__all__)

from subprocess import *

print(PIPE, STDOUT, DEVNULL, Popen.__name__)
print("list2cmdline" in globals())
print("fcntl" in globals(), "os" in globals())
