import _codecs
import warnings
warnings.filterwarnings('ignore', category=DeprecationWarning)

samples = [
    'ascii',
    'line\nnext',
    'quotes \' " \\',
    'high é • Ā',
    'emoji 😀',
]
for sample in samples:
    u = sample.encode('unicode_escape')
    r = sample.encode('raw_unicode_escape')
    print('round', repr(sample), u.decode('unicode_escape') == sample, r.decode('raw_unicode_escape') == sample)

payload = b'a\\n\\x41\\u00e9\\N{BULLET}\\q'
print('unicode-decode', repr(payload.decode('unicode_escape')))
print('direct-str-decode', repr(_codecs.unicode_escape_decode('\\u00e9')), repr(_codecs.raw_unicode_escape_decode('\\u00e9')))

raw_payload = b'a\\n\\x41\\u00e9\\U0001f600'
print('unicode-vs-raw', repr(raw_payload.decode('unicode_escape')), repr(raw_payload.decode('raw_unicode_escape')))

for sample in ['line\nAé•\\q😀', 'quotes \' " \\', 'latin1 \xff and high Ā']:
    print('encode-unicode', repr(sample.encode('unicode_escape')))
    print('encode-raw', repr(sample.encode('raw_unicode_escape')))

print('alias-encode', repr('é•'.encode('unicode-escape')))
print('alias-decode', repr(b'\\u00e9'.decode('unicode-escape')))

try:
    b'\\u00'.decode('unicode_escape')
except Exception as exc:
    print('truncated', type(exc).__name__, str(exc))
