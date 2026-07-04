"""Source-compatible fallback for CPython's private _heapq module."""

from heapq import __about__, heapify, heapify_max, heappop, heappop_max
from heapq import heappush, heappush_max, heappushpop, heappushpop_max
from heapq import heapreplace, heapreplace_max
