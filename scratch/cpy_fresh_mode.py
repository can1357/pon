p='/work/pon/tmp/cpy_fresh_mode_data.txt'
open(p,'w').close()
with open(p,'r+',encoding='utf-8') as f:
    print('mode', f.mode)
    print('__dict__', f.__dict__)
    try:
        del f.mode
        print('del fresh ok')
    except Exception as e:
        print('del fresh err', type(e).__name__, str(e))
    print('has after', hasattr(f, 'mode'))
