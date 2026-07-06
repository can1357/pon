import threading
import time

threads = [threading.Thread(target=time.sleep, args=(1,)) for _ in range(4)]
start = time.time()
for thread in threads:
    thread.start()
for thread in threads:
    thread.join()
elapsed = time.time() - start
print(round(elapsed, 2))
