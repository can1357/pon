import sys
sys.path.insert(0, 'vendored-meson/meson')
from pathlib import Path
print([str(p) for p in Path('/tmp/a/b').parents][:3])
