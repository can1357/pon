class Recorder:
    def __init__(self):
        self.events = []

    def __enter__(self):
        self.events.append("enter")
        return self

    def __exit__(self, exc_type, exc, tb):
        self.events.append("exit:" + (exc_type.__name__ if exc_type else "none"))
        return True

rec = Recorder()
with rec as active:
    active.events.append("body")
print(rec.events)
with rec:
    raise ValueError("hidden")
print(rec.events)
