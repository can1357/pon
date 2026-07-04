"""Source-compatible fallback for CPython's private _queue module."""


class Empty(Exception):
    """Exception raised by SimpleQueue.get(block=0)/get_nowait()."""


class SimpleQueue:
    """Simple, unbounded FIFO queue."""

    def __init__(self):
        self._queue = []

    def put(self, item, block=True, timeout=None):
        self._queue.append(item)

    def get(self, block=True, timeout=None):
        if timeout is not None and timeout < 0:
            raise ValueError("'timeout' must be a non-negative number")
        if not self._queue:
            raise Empty
        item = self._queue[0]
        del self._queue[0]
        return item

    def put_nowait(self, item):
        return self.put(item, block=False)

    def get_nowait(self):
        return self.get(block=False)

    def empty(self):
        return len(self._queue) == 0

    def qsize(self):
        return len(self._queue)
