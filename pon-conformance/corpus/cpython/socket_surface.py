import os
import socket
import _socket

# --- os._get_exports_list (the socket.py __all__ rung) ---------------------------
exports = os._get_exports_list(_socket)
print(type(exports) is list, len(exports) > 0)
print(all(not n.startswith("_") for n in exports))
print(exports == sorted(exports))
print("AF_INET" in exports, "gaierror" in exports, "socket" in exports)

class FakeModule:
    __all__ = ("b", "a", "c")

class NoAll:
    visible = 1
    _hidden = 2

print(os._get_exports_list(FakeModule()))
print(os._get_exports_list(NoAll()))

# --- constants are ints (IntEnum/IntFlag conversion) ------------------------------
print(isinstance(socket.AF_INET, int), isinstance(socket.SOCK_STREAM, int))
print(isinstance(socket.MSG_PEEK, int), isinstance(socket.AI_PASSIVE, int))
print(type(socket.AF_INET).__name__, type(socket.SOCK_STREAM).__name__)
print(type(socket.MSG_PEEK).__name__, type(socket.AI_PASSIVE).__name__)
print(socket.AF_INET.name, socket.SOCK_STREAM.name, socket.MSG_PEEK.name)
print(repr(socket.AF_INET), repr(socket.SOCK_DGRAM))

# Portable exact values (identical on every POSIX host).
print(int(socket.AF_UNSPEC), int(socket.AF_UNIX), int(socket.AF_INET))
print(int(socket.SOCK_STREAM), int(socket.SOCK_DGRAM), int(socket.SOCK_RAW))
print(int(socket.SHUT_RD), int(socket.SHUT_WR), int(socket.SHUT_RDWR))
print(int(socket.IPPROTO_IP), int(socket.IPPROTO_TCP), int(socket.IPPROTO_UDP))
print(int(socket.MSG_OOB), int(socket.MSG_PEEK), int(socket.INADDR_ANY))

# Members degrade to plain int arithmetic and format like ints.
print(socket.AF_INET + 1, socket.SOCK_STREAM * 3, socket.MSG_OOB | socket.MSG_PEEK)
print(str(socket.AF_INET), f"{socket.SOCK_STREAM}", str(socket.MSG_OOB | socket.MSG_PEEK))

# Enum identity round-trips.
print(socket.AddressFamily.AF_INET is socket.AF_INET)
print(socket.AddressFamily["AF_UNIX"] is socket.AF_UNIX)
print(socket.SocketKind(1) is socket.SOCK_STREAM)
print(socket.has_ipv6)

# --- exception surface: gaierror/herror catchable, aliases -----------------------
print(socket.error is OSError, socket.timeout is TimeoutError)
print(issubclass(socket.gaierror, OSError), issubclass(socket.herror, OSError))
print(socket.gaierror.__name__, socket.gaierror.__module__)
print(socket.herror.__name__, socket.herror.__module__)
print(socket.gaierror is _socket.gaierror, socket.herror is _socket.herror)

try:
    raise socket.gaierror(-2, "Name or service not known")
except socket.gaierror as exc:
    print("caught-gaierror", exc.args)
try:
    raise socket.gaierror(-5, "No address associated with hostname")
except OSError as exc:
    print("caught-as-OSError", type(exc).__name__, exc)
try:
    raise socket.herror(1, "Unknown host")
except socket.error as exc:
    print("caught-as-error-alias", type(exc).__name__, exc)
try:
    raise socket.gaierror(8, "nodename nor servname provided", "example.invalid")
except OSError as exc:
    print("caught-with-filename", exc)
try:
    raise socket.timeout("timed out")
except OSError as exc:
    print("caught-timeout", type(exc).__name__, exc)
try:
    try:
        raise OSError("plain")
    except socket.gaierror:
        print("WRONG: gaierror caught plain OSError")
except OSError as exc:
    print("gaierror-narrower-than-OSError", exc)

# --- socket type: subclassable, never instantiated here --------------------------
print(socket.socket.__name__, socket.socket.__module__)
print(issubclass(socket.socket, _socket.socket))

class _Probe(socket.socket):
    pass

print(_Probe.__name__, issubclass(_Probe, _socket.socket))
print(callable(socket.getaddrinfo), callable(socket.fromfd), callable(socket.getfqdn))

# --- default-timeout state (pure module state on both sides) ---------------------
print(socket.getdefaulttimeout())
socket.setdefaulttimeout(2.5)
print(socket.getdefaulttimeout())
socket.setdefaulttimeout(None)
print(socket.getdefaulttimeout())

# --- __all__ carries the _socket exports ------------------------------------------
names = set(socket.__all__)
print(all(n in names for n in [
    "AF_INET", "SOCK_STREAM", "gaierror", "herror", "has_ipv6", "getaddrinfo",
    "getdefaulttimeout", "setdefaulttimeout", "create_connection", "fromfd",
    "getfqdn", "socket", "error", "timeout", "SocketType", "AddressFamily",
    "SocketKind",
]))
