import sys

def check(label, first=None):
    mod = sys.modules.get('abc')
    print(label, mod is not None, hasattr(mod, 'ABCMeta') if mod is not None else None, hasattr(mod, 'abstractmethod') if mod is not None else None, (mod is first) if first is not None else 'NA')

import concurrent.futures
check('after_concurrent')
first = sys.modules.get('abc')
import importlib.resources
check('after_resources', first)
import logging.handlers
check('after_handlers', first)
import abc
print('direct_same', abc is first)
from abc import ABCMeta
print('from_same', ABCMeta is abc.ABCMeta)
import _collections_abc
print('collections_abc_same', _collections_abc.ABCMeta is abc.ABCMeta)
print('OK')
