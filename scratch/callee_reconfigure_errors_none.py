import tempfile
f=open(tempfile.mktemp(),'w+',encoding='utf-8')
f.reconfigure(errors='replace')
f.reconfigure(errors=None)
print(f.encoding, f.errors)
f.reconfigure(encoding='ascii', errors=None)
print(f.encoding, f.errors)
