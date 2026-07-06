import os
import signal
import threading
import time

threading.Thread(target=lambda: (time.sleep(0.2), os.kill(os.getpid(), signal.SIGINT))).start()
while True:
    pass
