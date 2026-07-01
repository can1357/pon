class OneShot:
    def __await__(self):
        received = yield "pause"
        return "got:" + received

async def run():
    print("async-start")
    value = await OneShot()
    print(value)
    return "done"

coro = run()
print(coro.send(None))
try:
    coro.send("value")
except StopIteration as exc:
    print("return", exc.value)
