import tempfile, os
p=os.path.join(tempfile.gettempdir(),'pon_handler.txt')
with open(p,'w',encoding='utf-8') as f:
    for e in ['bogus']:
        try:
            f.reconfigure(errors=e)
            print('reconfig ok', f.errors)
        except Exception as exc:
            print('reconfig error', type(exc).__name__, str(exc))
        try:
            print('write ret', f.write('x'))
        except Exception as exc:
            print('write error', type(exc).__name__, str(exc))
