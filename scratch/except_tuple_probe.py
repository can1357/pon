try:
    raise FileNotFoundError('x')
except (FileNotFoundError, PermissionError):
    print('caught')
