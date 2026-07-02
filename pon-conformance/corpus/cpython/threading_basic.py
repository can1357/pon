import threading

# Counter under a Lock: total is deterministic after join in both a real
# concurrent run (CPython) and pon's inline joinable-thread execution.
counter = 0
lock = threading.Lock()
seen_names = []


def work(n):
    global counter
    for _ in range(n):
        with lock:
            counter += 1
    with lock:
        seen_names.append(threading.current_thread().name)


threads = [threading.Thread(target=work, args=(250,)) for _ in range(4)]
for t in threads:
    t.start()
for t in threads:
    t.join()
print("counter", counter)
print("worker names", sorted(seen_names))
print("thread names", [t.name for t in threads])
print("alive after join", [t.is_alive() for t in threads])
print("idents set", all(t.ident is not None for t in threads))

# Main-thread bookkeeping.
main = threading.current_thread()
print("main name", main.name)
print("main is main_thread", main is threading.main_thread())
print("active after join", threading.active_count())

# Daemon flag round-trip (constructor and attribute).
d = threading.Thread(target=work, args=(0,), daemon=True)
print("daemon flag", d.daemon)
d.daemon = False
print("daemon cleared", d.daemon)

# Lifecycle errors.
try:
    threads[0].start()
except RuntimeError as exc:
    print("restart:", exc)
unstarted = threading.Thread(target=work, args=(0,))
try:
    unstarted.join()
except RuntimeError as exc:
    print("join-unstarted:", exc)

# Lock / RLock protocol.
plain = threading.Lock()
print("locked before", plain.locked())
print("acquire", plain.acquire())
print("locked held", plain.locked())
plain.release()
print("locked after", plain.locked())
r = threading.RLock()
print("rlock twice", r.acquire(), r.acquire())
r.release()
r.release()

# Event protocol (no blocking waits: set first).
event = threading.Event()
print("event initial", event.is_set())
event.set()
print("event set", event.is_set(), event.wait(0))
event.clear()
print("event cleared", event.is_set())
