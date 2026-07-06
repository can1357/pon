"""Source-compatible fallback for CPython's private _queue module."""

import threading
from collections import deque


class Empty(Exception):
    """Exception raised by SimpleQueue.get(block=0)/get_nowait()."""


class SimpleQueue:
    """Simple, unbounded FIFO queue."""

    def __init__(self):
        self._queue = deque()
        self._count = threading.Semaphore(0)

    def put(self, item, block=True, timeout=None):
        self._queue.append(item)
        self._count.release()

    def get(self, block=True, timeout=None):
        if timeout is not None and timeout < 0:
            raise ValueError("'timeout' must be a non-negative number")
        if not block:
            if not self._count.acquire(blocking=False):
                raise Empty
            return self._queue.popleft()
        if timeout is None:
            self._count.acquire()
            return self._queue.popleft()
        if not self._count.acquire(timeout=timeout):
            raise Empty
        return self._queue.popleft()

    def put_nowait(self, item):
        return self.put(item, block=False)

    def get_nowait(self):
        return self.get(block=False)

    def empty(self):
        return len(self._queue) == 0

    def qsize(self):
        return len(self._queue)
