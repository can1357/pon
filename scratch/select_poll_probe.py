import os
import select

r, w = os.pipe()
try:
    poller = select.poll()
    poller.register(r, select.POLLIN)
    print('poll_empty', poller.poll(0))
    os.write(w, b'x')
    ready = poller.poll(1000)
    print('poll_read', len(ready), ready[0][0] == r, bool(ready[0][1] & select.POLLIN))
    poller.modify(r, select.POLLIN | select.POLLHUP)
    poller.register(w, select.POLLOUT)
    ready = poller.poll(0)
    normalized = sorted((fd == r, fd == w, bool(mask & select.POLLIN), bool(mask & select.POLLOUT)) for fd, mask in ready)
    print('poll_both', normalized)
    poller.unregister(r)
    ready = poller.poll(0)
    print('poll_after_unregister', len(ready), ready[0][0] == w, bool(ready[0][1] & select.POLLOUT))
    rready, wready, xready = select.select([r], [w], [], 0)
    print('select_ready', rready == [r], wready == [w], xready == [])
finally:
    os.close(r)
    os.close(w)
