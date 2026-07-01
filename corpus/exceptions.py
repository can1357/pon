try:
    raise ValueError("boom")
except ValueError:
    print("caught")
