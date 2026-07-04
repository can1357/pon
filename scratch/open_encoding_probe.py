import tempfile, os
path = tempfile.mktemp()
with open(path, 'w', encoding='ASCII') as f:
    f.write('plain ascii\n')
with open(path, encoding='US-ASCII') as f:
    print(repr(f.read()), f.encoding)
with open(path, 'w', encoding='Latin-1') as f:
    f.write('caf\xe9\n')
with open(path, 'rb') as f:
    print(f.read())
with open(path, encoding='latin1') as f:
    print(repr(f.read()), f.encoding)
try:
    with open(path, 'w', encoding='ascii') as f:
        f.write('caf\xe9')
except UnicodeEncodeError as e:
    print('UEE', type(e).__name__)
try:
    open(path, encoding='klingon')
except (ValueError, LookupError) as e:
    print('bad-codec', 'klingon' in str(e))
os.unlink(path)
