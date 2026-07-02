# Derived from CPython v3.14.0 Lib/test/test_index.py topics (PSF license).

values = [10, 20, 30, 40, 50]
letters = "python"

print(values[False])
print(values[True])
print(values[True + True])
print(letters[False])
print(letters[True])
print(letters[True + True])

print(values[False:True + True])
print(values[True:4])
print(values[False:5:True + True])
print(letters[False:True + 4])
print(letters[True:5:True])
print(letters[False:6:True + True])

start = False
stop = True + True + True
step = True
print(values[start:stop:step])
print(letters[start:stop:step])

print(list(range(False, True + True + True, True)))
print(list(range(True, 6, True + True)))

for flag in [False, True]:
    print(flag, values[flag])
    print(flag, letters[flag])

print(values[bool(0):bool(1) + 3])
print(letters[bool(0):bool(1) + 3])
