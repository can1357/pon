import _codecs
for data in [b'\\xg', b'\\x4g', b'\\u0g', b'\\u00g', b'\\U0001f60g']:
    for fn in [_codecs.unicode_escape_decode, _codecs.raw_unicode_escape_decode]:
        try:
            print(fn.__name__, repr(data), repr(fn(data, 'strict', False)))
        except Exception as e:
            print(fn.__name__, repr(data), 'ERR', type(e).__name__, str(e))
