for m in ["sys","functools","difflib","pprint","re","warnings","collections","contextlib","traceback","time","types","unittest.result","unittest.util"]:
    __import__(m)
    print("OK", m)
