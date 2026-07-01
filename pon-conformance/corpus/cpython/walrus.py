values = [1, 2, 3, 4]
if (total := sum(values)) > 5:
    print("total", total)
while (item := values.pop()):
    print("pop", item)
    if item == 2:
        break
print(values)
