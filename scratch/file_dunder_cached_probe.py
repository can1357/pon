import os
print(__name__)
print(__file__)
print('__cached__' in globals())
print(globals().get('__cached__', 'MISSING'))
