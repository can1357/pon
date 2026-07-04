import hashlib
for name in ['md5', 'sha1', 'sha256', 'blake2b', 'sha3_256']:
    try:
        h = getattr(hashlib, name)(b'x').hexdigest()
        print(name, h[:8])
    except Exception as e:
        print(name, type(e).__name__, str(e))
