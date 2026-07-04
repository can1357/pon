import warnings
warnings.simplefilter('ignore')
for data in [b'\\', b'\\\n', b'\\\r\n', b'\\\r', b'\\0', b'\\07', b'\\008', b'\\400', b'\\777', b'\\78', b'\\xzz', b'\\xz1', b'\\uzzzz', b'\\Uzzzzzzzz']:
    try:
        print(repr(data), '->', repr(data.decode('unicode_escape')))
    except Exception as e:
        print(repr(data), 'ERR', type(e).__name__, str(e))
