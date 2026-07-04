import warnings
warnings.simplefilter('ignore')
for data in [b'\\x4z', b'\\xg4', b'\\u00g0', b'\\u0g00', b'\\U0001f60g', b'\\U0010ffff', b'\\U00110000']:
    for enc in ['unicode_escape','raw_unicode_escape']:
        try:
            print(enc, repr(data), '->', repr(data.decode(enc)))
        except Exception as e:
            print(enc, repr(data), 'ERR', type(e).__name__, str(e))
