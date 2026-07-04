#
#   Code output module
#


import cython
cython.declare(os=object, re=object, operator=object, textwrap=object,
               Template=object, Naming=object, Options=object, StringEncoding=object,
               Utils=object, SourceDescriptor=object, StringIOTree=object,
               DebugFlags=object, defaultdict=object,
               closing=object, partial=object, wraps=object,
               zlib_compress=object, bz2_compress=object, lzma_compress=object, zstd_compress=object)

import hashlib
import operator
import os
import re
import shutil
import textwrap
from dataclasses import dataclass
from string import Template
from functools import partial, wraps
from contextlib import closing, contextmanager
from collections import defaultdict

from Cython.Compiler import Naming
from Cython.Compiler import Options
from Cython.Compiler import DebugFlags
from Cython.Compiler import StringEncoding
from Cython import Utils
from Cython.Compiler.Scanning import SourceDescriptor
from Cython.StringIOTree import StringIOTree


# Set up available compression algorithms for maximum compression.
from zlib import compress as zlib_compress
try:
    from bz2 import compress as bz2_compress
except ImportError:
    bz2_compress = None
else:
    bz2_compress = partial(bz2_compress, compresslevel=9)
#try:
#    from lzma import compress as lzma_compress
#except ImportError:
#    lzma_compress = None
try:
    from compression.zstd import (
        compress as zstd_compress,
        CompressionParameter as zstd_CompressionParameter,
        Strategy as zstd_Strategy,
    )
except ImportError:
    zstd_compress = None
else:
    zstd_compress = partial(zstd_compress, options={
        zstd_CompressionParameter.strategy: zstd_Strategy.btultra2,
        zstd_CompressionParameter.compression_level: zstd_CompressionParameter.compression_level.bounds()[1],
    })
    del zstd_CompressionParameter
    del zstd_Strategy

compression_algorithms = [
    # Note: order is important and defines values for "CYTHON_COMPRESS_STRINGS" !
    (1, 'zlib', partial(zlib_compress, level=9)),
    (2, 'bz2', bz2_compress),
    (3, 'zstd', zstd_compress),
    # LZMA is difficult to configure for efficient output from C code
    # and the default output tends to be quite large.
    #(4, 'lzma', lzma_compress),
]


renamed_py2_builtins_map = {
    # builtins that had different names in Py2 code
    'unicode'    : 'str',
    'basestring' : 'str',
    'xrange'     : 'range',
    'raw_input'  : 'input',
}

ctypedef_builtins_map = {
    # types of builtins in "ctypedef class" statements which we don't
    # import either because the names conflict with C types or because
    # the type simply is not exposed.
    'py_int'             : '&PyLong_Type',
    'py_long'            : '&PyLong_Type',
    'py_float'           : '&PyFloat_Type',
    'wrapper_descriptor' : '&PyWrapperDescr_Type',
}

basicsize_builtins_map = {
    # builtins whose type has a different tp_basicsize than sizeof(...)
    'PyTypeObject': 'PyHeapTypeObject',
}

# Builtins as of Python version ...
KNOWN_PYTHON_BUILTINS_VERSION = (3, 15, 0, 'beta', 1)
KNOWN_PYTHON_BUILTINS = frozenset([
    'ArithmeticError',
    'AssertionError',
    'AttributeError',
    'BaseException',
    'BaseExceptionGroup',
    'BlockingIOError',
    'BrokenPipeError',
    'BufferError',
    'BytesWarning',
    'ChildProcessError',
    'ConnectionAbortedError',
    'ConnectionError',
    'ConnectionRefusedError',
    'ConnectionResetError',
    'DeprecationWarning',
    'EOFError',
    'Ellipsis',
    'EncodingWarning',
    'EnvironmentError',
    'Exception',
    'ExceptionGroup',
    'False',
    'FileExistsError',
    'FileNotFoundError',
    'FloatingPointError',
    'FutureWarning',
    'GeneratorExit',
    'IOError',
    'ImportCycleError',
    'ImportError',
    'ImportWarning',
    'IndentationError',
    'IndexError',
    'InterruptedError',
    'IsADirectoryError',
    'KeyError',
    'KeyboardInterrupt',
    'LookupError',
    'MemoryError',
    'ModuleNotFoundError',
    'NameError',
    'None',
    'NotADirectoryError',
    'NotImplemented',
    'NotImplementedError',
    'OSError',
    'OverflowError',
    'PendingDeprecationWarning',
    'PermissionError',
    'ProcessLookupError',
    'PythonFinalizationError',
    'RecursionError',
    'ReferenceError',
    'ResourceWarning',
    'RuntimeError',
    'RuntimeWarning',
    'StopAsyncIteration',
    'StopIteration',
    'SyntaxError',
    'SyntaxWarning',
    'SystemError',
    'SystemExit',
    'TabError',
    'TimeoutError',
    'True',
    'TypeError',
    'UnboundLocalError',
    'UnicodeDecodeError',
    'UnicodeEncodeError',
    'UnicodeError',
    'UnicodeTranslateError',
    'UnicodeWarning',
    'UserWarning',
    'ValueError',
    'Warning',
    'WindowsError',
    'ZeroDivisionError',
    '_IncompleteInputError',
    '__build_class__',
    '__debug__',
    '__lazy_import__',
    '__import__',
    'abs',
    'aiter',
    'all',
    'anext',
    'any',
    'ascii',
    'bin',
    'bool',
    'breakpoint',
    'bytearray',
    'bytes',
    'callable',
    'chr',
    'classmethod',
    'compile',
    'complex',
    'copyright',
    'credits',
    'delattr',
    'dict',
    'dir',
    'divmod',
    'enumerate',
    'eval',
    'exec',
    'exit',
    'filter',
    'float',
    'format',
    'frozendict',
    'frozenset',
    'getattr',
    'globals',
    'hasattr',
    'hash',
    'help',
    'hex',
    'id',
    'input',
    'int',
    'isinstance',
    'issubclass',
    'iter',
    'len',
    'license',
    'list',
    'locals',
    'map',
    'max',
    'memoryview',
    'min',
    'next',
    'object',
    'oct',
    'open',
    'ord',
    'pow',
    'print',
    'property',
    'quit',
    'range',
    'repr',
    'reversed',
    'round',
    'sentinel',
    'set',
    'setattr',
    'slice',
    'sorted',
    'staticmethod',
    'str',
    'sum',
    'super',
    'tuple',
    'type',
    'vars',
    'zip',
])
print('prefix 259 ok')
