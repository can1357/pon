m = {}.get
print(isinstance(m, object))
if not isinstance(m, object):
    object(m)
