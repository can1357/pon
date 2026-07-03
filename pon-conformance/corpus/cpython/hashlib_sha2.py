import logging
logging.disable(logging.CRITICAL)
import hashlib

cases = [
    ("sha224", hashlib.sha224, b""),
    ("sha256", hashlib.sha256, b"pon"),
    ("sha384", hashlib.sha384, b"pon\x00sha2"),
    ("sha512", hashlib.sha512, bytes(range(16))),
]

for expected_name, ctor, payload in cases:
    h = ctor(payload)
    digest = h.digest()
    print(expected_name, h.name, h.digest_size, h.block_size, len(digest), digest.hex() == h.hexdigest())
    print(h.hexdigest())
