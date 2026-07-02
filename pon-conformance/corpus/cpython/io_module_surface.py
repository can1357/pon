import builtins
import io

print(io.DEFAULT_BUFFER_SIZE)
print(type(io.DEFAULT_BUFFER_SIZE) is int)
print(io.SEEK_SET, io.SEEK_CUR, io.SEEK_END)

print(io.BlockingIOError is builtins.BlockingIOError)
print(io.BlockingIOError is BlockingIOError)
print(issubclass(io.BlockingIOError, OSError))
print(issubclass(io.UnsupportedOperation, OSError), issubclass(io.UnsupportedOperation, ValueError))

path = "/tmp/pon_io_module_surface.txt"
f = io.open(path, "w")
print(f.write("pon-io\n42\n"))
f.close()
print(f.closed)

with io.open(path, "r") as g:
    print(isinstance(g, io.TextIOWrapper))
    data = g.read()
print(data == "pon-io\n42\n")

h = io.open_code(path)
print(h.read() == b"pon-io\n42\n")
h.close()

print(io.text_encoding("ascii"))
enc = io.text_encoding(None)
print(enc == "locale" or enc == "utf-8")

print(issubclass(io.RawIOBase, io.IOBase))
print(issubclass(io.BufferedIOBase, io.IOBase))
print(issubclass(io.TextIOBase, io.IOBase))
print(issubclass(io.FileIO, io.RawIOBase))
print(issubclass(io.BytesIO, io.BufferedIOBase))
print(issubclass(io.BufferedReader, io.BufferedIOBase))
print(issubclass(io.BufferedWriter, io.BufferedIOBase))
print(issubclass(io.BufferedRandom, io.BufferedIOBase))
print(issubclass(io.BufferedRWPair, io.BufferedIOBase))
print(issubclass(io.StringIO, io.TextIOBase))
print(issubclass(io.TextIOWrapper, io.TextIOBase))
