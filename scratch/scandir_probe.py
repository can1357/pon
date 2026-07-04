import os
import pathlib
import shutil

root = pathlib.Path("tmp/scandir_fixture")
deleted_root = pathlib.Path("tmp/scandir_deleted_fixture")
dst = pathlib.Path("tmp/scandir_copy")
if dst.exists() or dst.is_symlink():
    shutil.rmtree(dst)

if hasattr(os, "symlink"):
    for path in (root, deleted_root):
        if path.exists() or path.is_symlink():
            shutil.rmtree(path)
    (root / "dir" / "nested").mkdir(parents=True)
    (root / "emptydir").mkdir()
    (root / "file.txt").write_text("hello\n", encoding="utf-8")
    (root / "dir" / "nested" / "inner.txt").write_text("inner\n", encoding="utf-8")
    os.symlink("dir", root / "link_dir")
    os.symlink("file.txt", root / "link_file")
    deleted_root.mkdir()
    os.symlink("victim_target.txt", deleted_root / "deleted_link")
else:
    required = [root / "dir", root / "emptydir", root / "file.txt", root / "link_dir", root / "link_file", deleted_root / "deleted_link"]
    if not all(path.exists() or path.is_symlink() for path in required):
        raise RuntimeError("scandir fixture must be precreated with symlinks")

with os.scandir(root) as it:
    entries = sorted(list(it), key=lambda entry: entry.name)
    print("context", it is iter(it))

for entry in entries:
    print(
        "entry",
        entry.name,
        entry.path,
        entry.is_dir(),
        entry.is_dir(follow_symlinks=False),
        entry.is_file(),
        entry.is_file(follow_symlinks=False),
        entry.is_symlink(),
        os.fspath(entry) == entry.path,
        entry.__fspath__() == entry.path,
        repr(entry),
    )

file_entry = next(entry for entry in entries if entry.name == "file.txt")
print("stat_size", file_entry.stat().st_size)

old_cwd = os.getcwd()
os.chdir(root)
try:
    with os.scandir() as default_it:
        print("default", ",".join(sorted(entry.name for entry in default_it)))
finally:
    os.chdir(old_cwd)

print("pathlib", ",".join(sorted(path.name for path in root.iterdir())))
shutil.copytree(root, dst)

def scan_relative(base):
    items = []
    files = []
    def visit(path, prefix):
        with os.scandir(path) as it:
            for entry in it:
                rel = entry.name if not prefix else prefix + "/" + entry.name
                items.append(rel)
                if entry.is_dir(follow_symlinks=False):
                    visit(entry.path, rel)
                elif entry.is_file(follow_symlinks=False):
                    files.append(rel)
    visit(base, "")
    return sorted(items), sorted(files)

copy_items, file_paths = scan_relative(dst)
print("copytree_items", len(copy_items), ",".join(copy_items))
print("copytree_files", len(file_paths), ",".join(file_paths))
print("copytree_bytes", (dst / "file.txt").read_text(encoding="utf-8") == "hello\n")

victim = deleted_root / "victim_target.txt"
victim.write_text("gone", encoding="utf-8")
with os.scandir(deleted_root) as it:
    deleted_entry = next(entry for entry in it if entry.name == "deleted_link")
victim.unlink()
print("deleted_is_file", deleted_entry.is_file())
try:
    deleted_entry.stat()
except FileNotFoundError as exc:
    print("deleted_stat", type(exc).__name__)
