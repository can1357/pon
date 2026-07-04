import builtins
import sys

checks = [
    ('_abc', ['_abc_init', '_abc_instancecheck', '_abc_register', '_abc_subclasscheck', '_get_dump', '_reset_caches', '_reset_registry', 'get_cache_token']),
    ('_operator', ['_compare_digest', 'abs', 'add', 'and_', 'attrgetter', 'call', 'concat', 'contains', 'countOf', 'delitem', 'eq', 'floordiv', 'ge', 'getitem', 'gt', 'iadd', 'iand', 'iconcat', 'ifloordiv', 'ilshift', 'imatmul', 'imod', 'imul', 'index', 'indexOf', 'inv', 'invert', 'ior', 'ipow', 'irshift', 'is_', 'is_none', 'is_not', 'is_not_none', 'isub', 'itemgetter', 'itruediv', 'ixor', 'le', 'length_hint', 'lshift', 'lt', 'matmul', 'methodcaller', 'mod', 'mul', 'ne', 'neg', 'not_', 'or_', 'pos', 'pow', 'rshift', 'setitem', 'sub', 'truediv', 'truth', 'xor']),
    ('_bisect', ['bisect_left', 'bisect_right', 'insort_left', 'insort_right']),
    ('_heapq', ['__about__', 'heapify', 'heapify_max', 'heappop', 'heappop_max', 'heappush', 'heappush_max', 'heappushpop', 'heappushpop_max', 'heapreplace', 'heapreplace_max']),
    ('_queue', ['Empty', 'SimpleQueue']),
    ('_stat', ['SF_APPEND', 'SF_ARCHIVED', 'SF_DATALESS', 'SF_FIRMLINK', 'SF_IMMUTABLE', 'SF_NOUNLINK', 'SF_SETTABLE', 'SF_SNAPSHOT', 'SF_SUPPORTED', 'SF_SYNTHETIC', 'ST_ATIME', 'ST_CTIME', 'ST_DEV', 'ST_GID', 'ST_INO', 'ST_MODE', 'ST_MTIME', 'ST_NLINK', 'ST_SIZE', 'ST_UID', 'S_ENFMT', 'S_IEXEC', 'S_IFBLK', 'S_IFCHR', 'S_IFDIR', 'S_IFDOOR', 'S_IFIFO', 'S_IFLNK', 'S_IFMT', 'S_IFPORT', 'S_IFREG', 'S_IFSOCK', 'S_IFWHT', 'S_IMODE', 'S_IREAD', 'S_IRGRP', 'S_IROTH', 'S_IRUSR', 'S_IRWXG', 'S_IRWXO', 'S_IRWXU', 'S_ISBLK', 'S_ISCHR', 'S_ISDIR', 'S_ISDOOR', 'S_ISFIFO', 'S_ISGID', 'S_ISLNK', 'S_ISPORT', 'S_ISREG', 'S_ISSOCK', 'S_ISUID', 'S_ISVTX', 'S_ISWHT', 'S_IWGRP', 'S_IWOTH', 'S_IWRITE', 'S_IWUSR', 'S_IXGRP', 'S_IXOTH', 'S_IXUSR', 'UF_APPEND', 'UF_COMPRESSED', 'UF_DATAVAULT', 'UF_HIDDEN', 'UF_IMMUTABLE', 'UF_NODUMP', 'UF_NOUNLINK', 'UF_OPAQUE', 'UF_SETTABLE', 'UF_TRACKED', 'filemode']),
    ('_types', ['AsyncGeneratorType', 'BuiltinFunctionType', 'BuiltinMethodType', 'CapsuleType', 'CellType', 'ClassMethodDescriptorType', 'CodeType', 'CoroutineType', 'EllipsisType', 'FrameType', 'FunctionType', 'GeneratorType', 'GenericAlias', 'GetSetDescriptorType', 'LambdaType', 'MappingProxyType', 'MemberDescriptorType', 'MethodDescriptorType', 'MethodType', 'MethodWrapperType', 'ModuleType', 'NoneType', 'NotImplementedType', 'SimpleNamespace', 'TracebackType', 'UnionType', 'WrapperDescriptorType']),
    ('cmath', ['acos', 'acosh', 'asin', 'asinh', 'atan', 'atanh', 'cos', 'cosh', 'e', 'exp', 'inf', 'infj', 'isclose', 'isfinite', 'isinf', 'isnan', 'log', 'log10', 'nan', 'nanj', 'phase', 'pi', 'polar', 'rect', 'sin', 'sinh', 'sqrt', 'tan', 'tanh', 'tau']),
]

metadata = set(['__builtins__', '__cached__', '__doc__', '__file__', '__loader__', '__name__', '__package__', '__spec__'])
for name, expected in checks:
    module = __import__(name)
    names = []
    for attr in dir(module):
        if attr not in metadata:
            names.append(attr)
    missing = []
    for attr in expected:
        if attr not in names:
            missing.append(attr)
    if missing:
        raise AssertionError(name + ' missing ' + repr(missing))
    print('OK', name)

if builtins.__debug__ is not True:
    raise AssertionError('__debug__ is not True')
if not sys.copyright.startswith('Copyright (c) 2001 Python Software Foundation.'):
    raise AssertionError('sys.copyright missing')

import _pylong
if float.fromhex('0x1.a934f0979a371p-2') <= 0.0:
    raise AssertionError('float.fromhex failed')

from _operator import _compare_digest
if not _compare_digest(b'abc', b'abc'):
    raise AssertionError('compare_digest true case failed')
if _compare_digest(b'abc', b'abd'):
    raise AssertionError('compare_digest false case failed')

for name in ['hmac', 'secrets', 'smtplib', 'site', 'imaplib']:
    __import__(name)
    print('OK', name)

print('SMOKE_OK')
