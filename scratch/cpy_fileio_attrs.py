p='/work/pon/tmp/cpy_fileio_data.bin'
open(p,'wb').close()
f = open(p,'rb')
try:
    print(type(f).__name__, hasattr(f,'encoding'))
    for name in ['encoding','errors','newlines','name','mode','closed','x']:
        try:
            setattr(f, name, 'v')
            print(name, 'set-ok', repr(getattr(f,name)))
        except Exception as e:
            print(name, 'set-err', type(e).__name__, str(e))
finally:
    f.close()
