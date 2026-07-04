import sys, tempfile, os
p=os.path.join(tempfile.gettempdir(),'pon_reconfigure_more.txt')
with open(p,'w',encoding='utf-8') as f:
    for kw in ({'encoding':1},{'encoding':'utf8'},{'encoding':'latin-1'},{'newline':1},{'newline':'x'},{'line_buffering':1},{'write_through':1}):
        try:
            r=f.reconfigure(**kw)
            print(kw, 'ok', r, f.encoding, f.errors, f.line_buffering, f.write_through)
        except Exception as e:
            print(kw, type(e).__name__, str(e))
