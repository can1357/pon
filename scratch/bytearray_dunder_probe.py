x = bytearray(b'abc')
print(x.__len__(), x.__contains__(98), x.__getitem__(slice(1, None)), type(x.__getitem__(slice(1, None))).__name__, list(x.__iter__()))
