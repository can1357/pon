print('pep562_mod body')
def __getattr__(name):
    print('pep562_mod __getattr__', name)
    if name == 'Lazy':
        return 'lazy-value'
    raise AttributeError(name)
