# Explicit dunder lookups on native containers (configparser's SectionProxy
# calls list.__iter__() directly), buffer writes through file objects
# (pickle's framer hands BytesIO.getbuffer() to the output file), the PEP 597
# "locale" encoding, and class-body staticmethod qualnames through pickle.
import io
import os
import pickle
import tempfile

lst = [3, 1, 2]
print(list(lst.__iter__()))
print(tuple((4, 5).__iter__()))
print(sorted({6, 7}.__iter__()))
print({'k': 1}.__contains__('k'))

with tempfile.TemporaryDirectory() as tmp:
    p = os.path.join(tmp, 'buf.bin')
    with open(p, 'wb') as fh:
        b = io.BytesIO()
        b.write(b'payload' * 3)
        print(fh.write(b.getbuffer()))
    print(open(p, 'rb').read()[:7])
    q = os.path.join(tmp, 'loc.txt')
    with open(q, 'w', encoding='locale') as fh:
        fh.write('hi')
    print(open(q, encoding='locale').read())


class Env:
    @staticmethod
    def _set(x):
        return x + 1


print(Env._set.__qualname__)
print(pickle.loads(pickle.dumps(Env._set))(41))
