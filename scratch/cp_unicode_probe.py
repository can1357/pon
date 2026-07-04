cases = [
    (b'\\x', 'unicode_escape'),
    (b'\\x4', 'unicode_escape'),
    (b'\\u00', 'unicode_escape'),
    (b'\\U0001', 'unicode_escape'),
    (b'\\U00110000', 'unicode_escape'),
    (b'\\N', 'unicode_escape'),
    (b'\\N{', 'unicode_escape'),
    (b'\\N{NO SUCH NAME}', 'unicode_escape'),
    (b'\\N{BULLET}', 'unicode_escape'),
    (b'\\q', 'unicode_escape'),
    (b'\\8', 'unicode_escape'),
    (b'\\777', 'unicode_escape'),
    (b'\\400', 'unicode_escape'),
    (b'\\999', 'unicode_escape'),
    (b'\\x41', 'raw_unicode_escape'),
    (b'\\u00e9', 'raw_unicode_escape'),
    (b'\\U0001f600', 'raw_unicode_escape'),
    (b'\\U00110000', 'raw_unicode_escape'),
    (b'\\u00', 'raw_unicode_escape'),
]
for data, enc in cases:
    try:
        print(enc, repr(data), '->', repr(data.decode(enc)))
    except Exception as e:
        print(enc, repr(data), 'ERR', type(e).__name__, str(e))

str_cases = ['abc\nAé•\\q', 'é', 'Ā', '😀', '\\', "'\"", ''.join(chr(i) for i in [0,7,8,9,10,11,12,13,31,32,33,126,127,160,255,256])]
for s in str_cases:
    for enc in ['unicode_escape','raw_unicode_escape']:
        try:
            print('ENC', enc, repr(s), '->', repr(s.encode(enc)))
        except Exception as e:
            print('ENC', enc, repr(s), 'ERR', type(e).__name__, str(e))

import codecs
for enc in ['unicode_escape','unicode-escape','raw_unicode_escape','raw-unicode-escape']:
    for obj in [b'\\u00e9', '\\u00e9']:
        try:
            print('codecs.decode', enc, type(obj).__name__, repr(codecs.decode(obj, enc)))
        except Exception as e:
            print('codecs.decode', enc, type(obj).__name__, 'ERR', type(e).__name__, str(e))

# direct _codecs arity/final
import _codecs
for final in [False, True]:
    try:
        print('_direct final', final, repr(_codecs.unicode_escape_decode(b'\\u00', 'strict', final)))
    except Exception as e:
        print('_direct final', final, 'ERR', type(e).__name__, str(e))
    try:
        print('_raw final', final, repr(_codecs.raw_unicode_escape_decode(b'\\u00', 'strict', final)))
    except Exception as e:
        print('_raw final', final, 'ERR', type(e).__name__, str(e))
