import threading
l = threading.Lock()
l.acquire()
l._at_fork_reinit()
print('unlocked after reinit:', not l.locked())
print(l.acquire(False))
l.release()
import concurrent.futures
print('cf ok')
