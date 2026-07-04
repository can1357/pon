import cProfile
p = cProfile.Profile()
p.enable()
for i in range(3):
    i * i
p.disable()
print(type(p.getstats()).__name__)
import pstats
import io
stats = pstats.Stats(p, stream=io.StringIO())
print('OK')
