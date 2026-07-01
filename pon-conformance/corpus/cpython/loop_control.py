out = []
for value in range(6):
    if value % 2 == 0:
        continue
    if value > 4:
        break
    out.append(value)
else:
    out.append(99)
print(out)

count = 0
while count < 3:
    count += 1
else:
    print("while-else", count)
