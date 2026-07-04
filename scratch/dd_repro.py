import collections
d = collections.defaultdict(list)
d['install'].extend(['x','y'])
print("defaultdict ok", dict(d))
