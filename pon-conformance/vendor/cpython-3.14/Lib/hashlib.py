#.  Copyright (C) 2005-2010   Gregory P. Smith (greg@krypto.org)
#  Licensed to PSF under a Contributor Agreement.
#

__doc__ = """hashlib module - A common interface to many hash functions.

new(name, data=b'', **kwargs) - returns a new hash object implementing the
                                given hash function; initializing the hash
                                using the given binary data.

Named constructor functions are also available, these are faster
than using new(name):

md5(), sha1(), sha224(), sha256(), sha384(), sha512(), blake2b(), blake2s(),
sha3_224, sha3_256, sha3_384, sha3_512, shake_128, and shake_256.

More algorithms may be available on your platform but the above are guaranteed
to exist.  See the algorithms_guaranteed and algorithms_available attributes
to find out what algorithm names can be passed to new().

NOTE: If you want the adler32 or crc32 hash functions they are available in
the zlib module.

Choose your hash function wisely.  Some have known collision weaknesses.
sha384 and sha512 will be slow on 32 bit platforms.

Hash objects have these methods:
 - update(data): Update the hash object with the bytes in data. Repeated calls
                 are equivalent to a single call with the concatenation of all
                 the arguments.
 - digest():     Return the digest of the bytes passed to the update() method
                 so far as a bytes object.
 - hexdigest():  Like digest() except the digest is returned as a string
                 of double length, containing only hexadecimal digits.
 - copy():       Return a copy (clone) of the hash object. This can be used to
                 efficiently compute the digests of data that share a common
                 initial substring.

For example, to obtain the digest of the byte string 'Nobody inspects the
spammish repetition':

    >>> import hashlib
    >>> m = hashlib.md5()
    >>> m.update(b"Nobody inspects")
    >>> m.update(b" the spammish repetition")
    >>> m.digest()
    b'\\xbbd\\x9c\\x83\\xdd\\x1e\\xa5\\xc9\\xd9\\xde\\xc9\\xa1\\x8d\\xf0\\xff\\xe9'

More condensed:

    >>> hashlib.sha224(b"Nobody inspects the spammish repetition").hexdigest()
    'a4337bc45a8fc544c03f52dc550cd6e1e87021bc896588bd79e901e2'

"""

# This tuple and __get_builtin_constructor() must be modified if a new
# always available algorithm is added.
__always_supported = ('md5', 'sha1', 'sha224', 'sha256', 'sha384', 'sha512',
                      'blake2b', 'blake2s',
                      'sha3_224', 'sha3_256', 'sha3_384', 'sha3_512',
                      'shake_128', 'shake_256')


algorithms_guaranteed = set(__always_supported)
algorithms_available = set(__always_supported)

__all__ = __always_supported + ('new', 'algorithms_guaranteed',
                                'algorithms_available', 'file_digest')


__builtin_constructor_cache = {}

# Prefer our blake2 implementation
# OpenSSL 1.1.0 comes with a limited implementation of blake2b/s. The OpenSSL
# implementations neither support keyed blake2 (blake2 MAC) nor advanced
# features like salt, personalization, or tree hashing. OpenSSL hash-only
# variants are available as 'blake2b512' and 'blake2s256', though.
__block_openssl_constructor = {
    'blake2b', 'blake2s',
}

def __get_builtin_constructor(name):
    cache = __builtin_constructor_cache
    constructor = cache.get(name)
    if constructor is not None:
        return constructor
    try:
        if name in {'SHA1', 'sha1'}:
            import _sha1
            cache['SHA1'] = cache['sha1'] = _sha1.sha1
        elif name in {'MD5', 'md5'}:
            import _md5
            cache['MD5'] = cache['md5'] = _md5.md5
        elif name in {'SHA256', 'sha256', 'SHA224', 'sha224'}:
            import _sha2
            cache['SHA224'] = cache['sha224'] = _sha2.sha224
            cache['SHA256'] = cache['sha256'] = _sha2.sha256
        elif name in {'SHA512', 'sha512', 'SHA384', 'sha384'}:
            import _sha2
            cache['SHA384'] = cache['sha384'] = _sha2.sha384
            cache['SHA512'] = cache['sha512'] = _sha2.sha512
        elif name in {'blake2b', 'blake2s'}:
            import _blake2
            cache['blake2b'] = _blake2.blake2b
            cache['blake2s'] = _blake2.blake2s
        elif name in {'sha3_224', 'sha3_256', 'sha3_384', 'sha3_512'}:
            import _sha3
            cache['sha3_224'] = _sha3.sha3_224
            cache['sha3_256'] = _sha3.sha3_256
            cache['sha3_384'] = _sha3.sha3_384
            cache['sha3_512'] = _sha3.sha3_512
        elif name in {'shake_128', 'shake_256'}:
            import _sha3
            cache['shake_128'] = _sha3.shake_128
            cache['shake_256'] = _sha3.shake_256
    except ImportError:
        pass  # no extension module, this hash is unsupported.

    constructor = cache.get(name)
    if constructor is not None:
        return constructor

    raise ValueError('unsupported hash type ' + name)


def __get_openssl_constructor(name):
    if name in __block_openssl_constructor:
        # Prefer our builtin blake2 implementation.
        return __get_builtin_constructor(name)
    try:
        # MD5, SHA1, and SHA2 are in all supported OpenSSL versions
        # SHA3/shake are available in OpenSSL 1.1.1+
        f = getattr(_hashlib, 'openssl_' + name)
        # Allow the C module to raise ValueError.  The function will be
        # defined but the hash not actually available.  Don't fall back to
        # builtin if the current security policy blocks a digest, bpo#40695.
        f(usedforsecurity=False)
        # Use the C function directly (very fast)
        return f
    except (AttributeError, ValueError):
        return __get_builtin_constructor(name)


def __py_new(name, *args, **kwargs):
    """new(name, data=b'', **kwargs) - Return a new hashing object using the
    named algorithm; optionally initialized with data (which must be
    a bytes-like object).
    """
    return __get_builtin_constructor(name)(*args, **kwargs)


def __hash_new(name, *args, **kwargs):
    """new(name, data=b'') - Return a new hashing object using the named algorithm;
    optionally initialized with data (which must be a bytes-like object).
    """
    if name in __block_openssl_constructor:
        # Prefer our builtin blake2 implementation.
        return __get_builtin_constructor(name)(*args, **kwargs)
    try:
        return _hashlib.new(name, *args, **kwargs)
    except ValueError:
        # If the _hashlib module (OpenSSL) doesn't support the named
        # hash, try using our builtin implementations.
        # This allows for SHA224/256 and SHA384/512 support even though
        # the OpenSSL library prior to 0.9.8 doesn't provide them.
        return __get_builtin_constructor(name)(*args, **kwargs)


try:
    import _hashlib
    new = __hash_new
    __get_hash = __get_openssl_constructor
    algorithms_available = algorithms_available.union(
            _hashlib.openssl_md_meth_names)
except ImportError:
    _hashlib = None
    new = __py_new
    __get_hash = __get_builtin_constructor

try:
    # OpenSSL's PKCS5_PBKDF2_HMAC requires OpenSSL 1.0+ with HMAC and SHA
    from _hashlib import pbkdf2_hmac
    __all__ += ('pbkdf2_hmac',)
except ImportError:
    pass


try:
    # OpenSSL's scrypt requires OpenSSL 1.1+
    from _hashlib import scrypt  # noqa: F401
except ImportError:
    pass


if 'pbkdf2_hmac' not in globals():
    def pbkdf2_hmac(hash_name, password, salt, iterations, dklen=None):
        """Password based key derivation function 2 (PKCS #5 v2.0)."""
        from operator import index as _index
        import hmac as _hmac

        if not isinstance(hash_name, str):
            raise TypeError("hash_name must be a string")
        password = bytes(password)
        salt = bytes(salt)
        iterations = _index(iterations)
        if iterations < 1:
            raise ValueError("iteration value must be greater than 0.")

        digest = new(hash_name)
        hlen = digest.digest_size
        if dklen is None:
            dklen = hlen
        else:
            dklen = _index(dklen)
            if dklen < 1:
                raise ValueError("key length must be greater than 0.")

        blocks, extra = divmod(dklen, hlen)
        if extra:
            blocks += 1
        if blocks > 0xffffffff:
            raise OverflowError("derived key too long")

        out = bytearray()
        for block_index in range(1, blocks + 1):
            u = _hmac.digest(password, salt + block_index.to_bytes(4, 'big'), hash_name)
            acc = bytearray(u)
            for _ in range(iterations - 1):
                u = _hmac.digest(password, u, hash_name)
                for i, value in enumerate(u):
                    acc[i] ^= value
            out.extend(acc)
        return bytes(out[:dklen])

    __all__ += ('pbkdf2_hmac',)


if 'scrypt' not in globals():
    def _scrypt_rotl(value, shift):
        return ((value << shift) | (value >> (32 - shift))) & 0xffffffff


    def _scrypt_salsa208(block):
        words = [int.from_bytes(block[i:i + 4], 'little') for i in range(0, 64, 4)]
        x = words[:]
        for _ in range(4):
            x[4] ^= _scrypt_rotl((x[0] + x[12]) & 0xffffffff, 7)
            x[8] ^= _scrypt_rotl((x[4] + x[0]) & 0xffffffff, 9)
            x[12] ^= _scrypt_rotl((x[8] + x[4]) & 0xffffffff, 13)
            x[0] ^= _scrypt_rotl((x[12] + x[8]) & 0xffffffff, 18)
            x[9] ^= _scrypt_rotl((x[5] + x[1]) & 0xffffffff, 7)
            x[13] ^= _scrypt_rotl((x[9] + x[5]) & 0xffffffff, 9)
            x[1] ^= _scrypt_rotl((x[13] + x[9]) & 0xffffffff, 13)
            x[5] ^= _scrypt_rotl((x[1] + x[13]) & 0xffffffff, 18)
            x[14] ^= _scrypt_rotl((x[10] + x[6]) & 0xffffffff, 7)
            x[2] ^= _scrypt_rotl((x[14] + x[10]) & 0xffffffff, 9)
            x[6] ^= _scrypt_rotl((x[2] + x[14]) & 0xffffffff, 13)
            x[10] ^= _scrypt_rotl((x[6] + x[2]) & 0xffffffff, 18)
            x[3] ^= _scrypt_rotl((x[15] + x[11]) & 0xffffffff, 7)
            x[7] ^= _scrypt_rotl((x[3] + x[15]) & 0xffffffff, 9)
            x[11] ^= _scrypt_rotl((x[7] + x[3]) & 0xffffffff, 13)
            x[15] ^= _scrypt_rotl((x[11] + x[7]) & 0xffffffff, 18)
            x[1] ^= _scrypt_rotl((x[0] + x[3]) & 0xffffffff, 7)
            x[2] ^= _scrypt_rotl((x[1] + x[0]) & 0xffffffff, 9)
            x[3] ^= _scrypt_rotl((x[2] + x[1]) & 0xffffffff, 13)
            x[0] ^= _scrypt_rotl((x[3] + x[2]) & 0xffffffff, 18)
            x[6] ^= _scrypt_rotl((x[5] + x[4]) & 0xffffffff, 7)
            x[7] ^= _scrypt_rotl((x[6] + x[5]) & 0xffffffff, 9)
            x[4] ^= _scrypt_rotl((x[7] + x[6]) & 0xffffffff, 13)
            x[5] ^= _scrypt_rotl((x[4] + x[7]) & 0xffffffff, 18)
            x[11] ^= _scrypt_rotl((x[10] + x[9]) & 0xffffffff, 7)
            x[8] ^= _scrypt_rotl((x[11] + x[10]) & 0xffffffff, 9)
            x[9] ^= _scrypt_rotl((x[8] + x[11]) & 0xffffffff, 13)
            x[10] ^= _scrypt_rotl((x[9] + x[8]) & 0xffffffff, 18)
            x[12] ^= _scrypt_rotl((x[15] + x[14]) & 0xffffffff, 7)
            x[13] ^= _scrypt_rotl((x[12] + x[15]) & 0xffffffff, 9)
            x[14] ^= _scrypt_rotl((x[13] + x[12]) & 0xffffffff, 13)
            x[15] ^= _scrypt_rotl((x[14] + x[13]) & 0xffffffff, 18)
        return b''.join(((x[i] + words[i]) & 0xffffffff).to_bytes(4, 'little')
                        for i in range(16))


    def _scrypt_blockmix(block, r):
        x = block[-64:]
        y = []
        for i in range(2 * r):
            chunk = block[i * 64:(i + 1) * 64]
            x = _scrypt_salsa208(bytes(a ^ b for a, b in zip(x, chunk)))
            y.append(x)
        return b''.join(y[::2] + y[1::2])


    def _scrypt_integerify(block, r):
        return int.from_bytes(block[(2 * r - 1) * 64:(2 * r - 1) * 64 + 8],
                              'little')


    def _scrypt_romix(block, n, r):
        x = block
        v = []
        for _ in range(n):
            v.append(x)
            x = _scrypt_blockmix(x, r)
        for _ in range(n):
            j = _scrypt_integerify(x, r) & (n - 1)
            x = _scrypt_blockmix(bytes(a ^ b for a, b in zip(x, v[j])), r)
        return x


    def scrypt(password, *, salt, n, r, p, maxmem=0, dklen=64):
        """Derive a key from *password* using the scrypt KDF."""
        from operator import index as _index

        password = bytes(password)
        salt = bytes(salt)
        n = _index(n)
        r = _index(r)
        p = _index(p)
        maxmem = _index(maxmem)
        dklen = _index(dklen)
        if n <= 1 or n & (n - 1):
            raise ValueError("n must be a power of 2.")
        if r <= 0 or p <= 0:
            raise ValueError("r and p must be positive.")
        if dklen <= 0:
            raise ValueError("dklen must be greater than 0.")
        memory = 128 * n * r
        if maxmem and memory > maxmem:
            raise ValueError("memory limit exceeded")

        block_size = 128 * r
        b = pbkdf2_hmac('sha256', password, salt, 1, p * block_size)
        mixed = [
            _scrypt_romix(b[i * block_size:(i + 1) * block_size], n, r)
            for i in range(p)
        ]
        return pbkdf2_hmac('sha256', password, b''.join(mixed), 1, dklen)

    __all__ += ('scrypt',)


def file_digest(fileobj, digest, /, *, _bufsize=2**18):
    """Hash the contents of a file-like object. Returns a digest object.

    *fileobj* must be a file-like object opened for reading in binary mode.
    It accepts file objects from open(), io.BytesIO(), and SocketIO objects.
    The function may bypass Python's I/O and use the file descriptor *fileno*
    directly.

    *digest* must either be a hash algorithm name as a *str*, a hash
    constructor, or a callable that returns a hash object.
    """
    # On Linux we could use AF_ALG sockets and sendfile() to archive zero-copy
    # hashing with hardware acceleration.
    if isinstance(digest, str):
        digestobj = new(digest)
    else:
        digestobj = digest()

    if hasattr(fileobj, "getbuffer"):
        # io.BytesIO object, use zero-copy buffer
        digestobj.update(fileobj.getbuffer())
        return digestobj

    # Only binary files implement readinto().
    if not (
        hasattr(fileobj, "readinto")
        and hasattr(fileobj, "readable")
        and fileobj.readable()
    ):
        raise ValueError(
            f"'{fileobj!r}' is not a file-like object in binary reading mode."
        )

    # binary file, socket.SocketIO object
    # Note: socket I/O uses different syscalls than file I/O.
    buf = bytearray(_bufsize)  # Reusable buffer to reduce allocations.
    view = memoryview(buf)
    while True:
        size = fileobj.readinto(buf)
        if size is None:
            raise BlockingIOError("I/O operation would block.")
        if size == 0:
            break  # EOF
        digestobj.update(view[:size])

    return digestobj


for __func_name in __always_supported:
    # try them all, some may not work due to the OpenSSL
    # version not supporting that algorithm.
    try:
        globals()[__func_name] = __get_hash(__func_name)
    except ValueError:
        import logging
        logging.exception('code for hash %s was not found.', __func_name)


# Cleanup locals()
del __always_supported, __func_name, __get_hash
del __py_new, __hash_new, __get_openssl_constructor
