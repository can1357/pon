import _codecs
for s in ['é', '€', '😀', '\\xe9']:
    for fn in [_codecs.unicode_escape_decode, _codecs.raw_unicode_escape_decode]:
        try:
            print(fn.__name__, repr(s), '->', repr(fn(s)))
        except Exception as e:
            print(fn.__name__, repr(s), 'ERR', type(e).__name__, str(e))
