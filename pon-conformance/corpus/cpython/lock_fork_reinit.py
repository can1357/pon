# Lock at-fork reinitialization and concurrent.futures import surface.
import os
import threading

lock = threading.Lock()
print("initial", lock.locked())
print("acquire", lock.acquire())
print("locked", lock.locked())
lock._at_fork_reinit()
print("after reinit", lock.locked())
print("reacquire", lock.acquire(False))
lock.release()
print("released", lock.locked())
print("register at fork", hasattr(os, "register_at_fork"))

import concurrent.futures

print("concurrent", "ok")
