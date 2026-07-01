def append_item(item, bucket=None, scale=2):
    if bucket is None:
        bucket = []
    bucket.append(item * scale)
    return bucket

print(append_item(3))
print(append_item(4, [1], 3))
