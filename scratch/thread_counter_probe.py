import threading

count = 0
lock = threading.Lock()

def worker():
    global count
    for _ in range(25):
        lock.acquire()
        count += 1
        lock.release()

threads = [threading.Thread(target=worker) for _ in range(4)]
for thread in threads:
    thread.start()
for thread in threads:
    thread.join()
print(count)
