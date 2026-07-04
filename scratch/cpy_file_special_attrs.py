p='/work/pon/tmp/cpy_special_data.txt'
open(p,'w').close()
with open(p,'r+',encoding='utf-8') as f:
    for name in ['encoding','errors','newlines']:
        try:
            setattr(f, name, 'v')
            print(name, 'set-ok', repr(getattr(f,name)))
        except Exception as e:
            print(name, 'set-err', type(e).__name__, str(e))
        try:
            delattr(f, name)
            print(name, 'del-ok')
        except Exception as e:
            print(name, 'del-err', type(e).__name__, str(e))
