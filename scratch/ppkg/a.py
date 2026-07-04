class _PathParents:
    def __init__(self, path):
        self.path = path
    def __repr__(self):
        return 'parents:' + self.path.name

class PurePath:
    def __init__(self, name='ok'):
        self.name = name
    @property
    def parents(self):
        return _PathParents(self)

def from_func():
    return PurePath('func').parents
