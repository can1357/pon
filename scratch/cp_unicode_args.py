import _codecs, codecs
for fn in [_codecs.unicode_escape_decode, _codecs.raw_unicode_escape_decode]:
    for obj in ['\\u00e9', bytearray(b'\\u00e9'), memoryview(b'\\u00e9'), 123]:
        try:
            print(fn.__name__, type(obj).__name__, repr(fn(obj)))
        except Exception as e:
            print(fn.__name__, type(obj).__name__, 'ERR', type(e).__name__, str(e))
for fn in [_codecs.unicode_escape_encode, _codecs.raw_unicode_escape_encode]:
    for obj in ['é', b'\xc3\xa9', 123]:
        try:
            print(fn.__name__, type(obj).__name__, repr(fn(obj)))
        except Exception as e:
            print(fn.__name__, type(obj).__name__, 'ERR', type(e).__name__, str(e))
for enc in ['unicode_escape', 'raw_unicode_escape']:
    try:
        print('clean bogus encode', enc, repr('ok'.encode(enc, 'bogus')))
    except Exception as e:
        print('clean bogus encode', enc, 'ERR', type(e).__name__, str(e))
