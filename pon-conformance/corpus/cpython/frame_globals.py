# sys._getframe(depth).f_globals surface: function frames resolve the
# caller's module globals at two call depths, a missing frame attribute
# raises a catchable AttributeError, and the returned mapping is the live
# module namespace — mutations through it surface as module globals
# (CPython: f_globals IS the module dict).
import sys

def depth1_name():
    return sys._getframe(1).f_globals.get('__name__', '<missing>')

def call_depth1():
    return depth1_name()

def depth2_name():
    return sys._getframe(2).f_globals.get('__name__', '<missing>')

def call_depth2():
    return depth2_name()

print(depth1_name())
print(call_depth1())
print(call_depth2())
print(type(sys._getframe(0).f_globals).__name__)

def missing_attr():
    frame = sys._getframe(0)
    try:
        return frame.f_nosuch
    except AttributeError:
        return 'caught'

print(missing_attr())

MARKER = 'before'
sys._getframe(0).f_globals['MARKER'] = 'after'
print(MARKER)
print(sys._getframe(0).f_globals.get('MARKER'))
