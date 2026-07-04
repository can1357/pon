import _codecs
for data in [b'\\', b'\\x', b'\\N', b'\\N{', b'\\N{NO']:
    for fn in [_codecs.unicode_escape_decode, _codecs.raw_unicode_escape_decode]:
        try:
            print(fn.__name__, repr(data), repr(fn(data, 'strict', False)))
        except Exception as e:
            print(fn.__name__, repr(data), 'ERR', type(e).__name__, str(e))
