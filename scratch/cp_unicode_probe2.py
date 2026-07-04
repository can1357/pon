import _codecs, warnings
warnings.simplefilter('ignore')
for data in [b'a\\u00', b'a\\x4', b'a\\U0001', b'a\\N{BUL']:
    for enc, fn in [('unicode', _codecs.unicode_escape_decode), ('raw', _codecs.raw_unicode_escape_decode)]:
        for final in [False, True]:
            try:
                print(enc, repr(data), final, repr(fn(data, 'strict', final)))
            except Exception as e:
                print(enc, repr(data), final, 'ERR', type(e).__name__, str(e))

for errors in ['ignore','replace','backslashreplace']:
    for enc, fn in [('unicode', _codecs.unicode_escape_decode), ('raw', _codecs.raw_unicode_escape_decode)]:
        try:
            print('errh', enc, errors, repr(fn(b'a\\u00b', errors, True)))
        except Exception as e:
            print('errh', enc, errors, 'ERR', type(e).__name__, str(e))
