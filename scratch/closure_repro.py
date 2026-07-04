def _table(scheme):
    def func(value, name):
        table = {}
        for key, val in value.items():
            check = scheme.get(key)
            if check is None:
                raise ValueError(f'Unknown {name}.{key}')
            table[key] = check(val, f'{name}.{key}')
        return table
    return func

def _strings(value, name):
    return value

scheme = _table({
    'wheel': _table({'exclude': _strings, 'include': _strings}),
})
print(scheme({'wheel': {'include': ['a', 'b']}}, 'tool.meson-python'))
