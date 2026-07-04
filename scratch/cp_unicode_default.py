import _codecs
for fn in [_codecs.unicode_escape_decode, _codecs.raw_unicode_escape_decode]:
    try:
        print(fn.__name__, repr(fn(b'\\u00')))
    except Exception as e:
        print(fn.__name__, 'ERR', type(e).__name__, str(e))
