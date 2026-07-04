import cProfile
p = cProfile.Profile()
p.enable()
for i in range(3):
    i * i
p.disable()
print(p.getstats())
