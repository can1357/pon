print('pkg body')
def __getattr__(name):
    print('pkg __getattr__', name)
    if name == 'Lazy':
        return 'pkg-lazy'
    raise AttributeError(name)
