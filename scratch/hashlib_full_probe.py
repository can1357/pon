import hashlib

for name in ['md5', 'sha1', 'sha256', 'sha3_256', 'blake2b', 'blake2s']:
    print('digest', name, getattr(hashlib, name)(b'abc').hexdigest())

print('shake_128_8', hashlib.shake_128(b'abc').hexdigest(8))
print('blake2b_16', hashlib.blake2b(b'abc', digest_size=16).hexdigest())
print('blake2s_keyed', hashlib.blake2s(b'abc', key=b'key').hexdigest())
print('blake2s_keyed_16', hashlib.blake2s(b'abc', digest_size=16, key=b'key').hexdigest())
print('blake2b_keyed_16', hashlib.blake2b(b'abc', digest_size=16, key=b'key').hexdigest())
print('new_md5', hashlib.new('md5', b'x').hexdigest())

left = hashlib.md5(b'a')
right = left.copy()
left.update(b'b')
right.update(b'c')
print('copy_md5', left.hexdigest(), right.hexdigest())

for label, obj in [
    ('md5', hashlib.md5()),
    ('sha1', hashlib.sha1()),
    ('sha256', hashlib.sha256()),
    ('sha3_256', hashlib.sha3_256()),
    ('shake_128', hashlib.shake_128()),
    ('blake2b', hashlib.blake2b()),
    ('blake2b_16', hashlib.blake2b(digest_size=16)),
    ('blake2s', hashlib.blake2s()),
]:
    print('attrs', label, obj.name, obj.digest_size, obj.block_size)

required = ['md5', 'sha1', 'sha224', 'sha256', 'sha384', 'sha512',
            'blake2b', 'blake2s', 'sha3_224', 'sha3_256', 'sha3_384',
            'sha3_512', 'shake_128', 'shake_256']
print('available', [(name, name in hashlib.algorithms_available) for name in required])
