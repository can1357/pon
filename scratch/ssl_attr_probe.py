import _ssl
import ssl
import ftplib
import socket

required = [
    'ALERT_DESCRIPTION_ACCESS_DENIED',
    'ALERT_DESCRIPTION_BAD_RECORD_MAC',
    'ALERT_DESCRIPTION_UNSUPPORTED_EXTENSION',
    'ENCODING_DER',
    'ENCODING_PEM',
    'HAS_TLS_UNIQUE',
    'HOSTFLAG_NEVER_CHECK_SUBJECT',
    'OP_NO_COMPRESSION',
    'OP_IGNORE_UNEXPECTED_EOF',
    'PROTO_SSLv3',
    'PROTO_TLSv1',
    'PROTO_TLSv1_1',
    'PROTOCOL_TLSv1',
    'PROTOCOL_TLSv1_1',
    'PROTOCOL_TLSv1_2',
    'VERIFY_CRL_CHECK_CHAIN',
    'VERIFY_X509_TRUSTED_FIRST',
    'get_default_verify_paths',
    'nid2obj',
    'txt2obj',
]
missing = [name for name in required if not hasattr(_ssl, name)]
print('missing', missing)
assert missing == []

print('ftplib_tls', hasattr(ftplib, 'FTP_TLS'))
assert hasattr(ftplib, 'FTP_TLS')
assert ftplib._SSLSocket is ssl.SSLSocket

print('ssl_wrapper', ssl.SSLContext, ssl.SSLSocket, ssl._create_stdlib_context)
ctx = ssl._create_stdlib_context()
print('ctx', isinstance(ctx, ssl.SSLContext), ctx.protocol, ctx.verify_mode, ctx.check_hostname)
assert isinstance(ctx, ssl.SSLContext)
assert ctx.verify_mode == ssl.CERT_NONE
assert ctx.check_hostname is False

rand = _ssl.RAND_bytes(8)
print('rand_len', len(rand), _ssl.RAND_status())
assert len(rand) == 8
assert _ssl.RAND_status() is True
print('version', _ssl.OPENSSL_VERSION, _ssl.OPENSSL_VERSION_INFO)
assert isinstance(_ssl.OPENSSL_VERSION, str)
assert isinstance(_ssl.OPENSSL_VERSION_INFO, tuple)
assert _ssl.txt2obj('1.3.6.1.5.5.7.3.1', name=False)[1] == 'serverAuth'
assert _ssl.nid2obj(129)[3] == '1.3.6.1.5.5.7.3.1'
paths = _ssl.get_default_verify_paths()
print('paths', paths)
assert len(paths) == 4

sock = socket.socket()
try:
    wrapped = ctx.wrap_socket(sock, do_handshake_on_connect=False)
    print('wrapped', isinstance(wrapped, ssl.SSLSocket), wrapped._sslobj is None)
    assert isinstance(wrapped, ssl.SSLSocket)
    assert wrapped._sslobj is None
finally:
    try:
        wrapped.close()
    except NameError:
        sock.close()

print('ok')
