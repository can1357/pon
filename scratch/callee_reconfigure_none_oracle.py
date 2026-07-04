import tempfile
f=open(tempfile.mktemp(),'w+',encoding='utf-8')
f.reconfigure(errors='replace')
f.reconfigure(encoding=None)
print(f.encoding, f.errors)
