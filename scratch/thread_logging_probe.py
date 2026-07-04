import _thread
print('thread attrs', [name for name in ('_ExceptHookArgs','_excepthook','allocate','allocate_lock','start_new','start_new_thread') if hasattr(_thread,name)])
print('allocate identity', getattr(_thread,'allocate',None) is getattr(_thread,'allocate_lock',None))
print('start_new identity', getattr(_thread,'start_new',None) is getattr(_thread,'start_new_thread',None))
try:
    import logging, os
    print('has register_at_fork', hasattr(os, 'register_at_fork'))
    print('logging attrs', hasattr(logging, '_after_at_fork_child_reinit_locks'), hasattr(logging, '_at_fork_reinit_lock_weakset'))
except Exception as exc:
    print('logging import err', type(exc).__name__, exc)
