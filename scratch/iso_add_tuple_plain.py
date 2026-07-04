try:
    x = [1] + (2,)
except TypeError as e:
    print('TE', e)
