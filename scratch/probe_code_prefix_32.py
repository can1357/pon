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
print('prefix 32 ok')
