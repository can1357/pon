from pathlib import Path
print([str(p) for p in Path('/tmp/a/b').parents][:3])
