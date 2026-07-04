# subprocess input delivery and stdout round trips.
import subprocess

binary = subprocess.run(["cat"], input=b"hello", capture_output=True)
print("bytes", binary.returncode, binary.stdout == b"hello", binary.stderr == b"")

text = subprocess.run(["tr", "a-z", "A-Z"], input="hello\n", capture_output=True, text=True)
print("text", text.returncode, text.stdout == "HELLO\n", text.stderr == "")

process = subprocess.Popen(["cat"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
stdout, stderr = process.communicate(b"communicate")
print("communicate", process.returncode, stdout == b"communicate", stderr == b"")

large = b"x" * (70 * 1024)
large_run = subprocess.run(["cat"], input=large, capture_output=True)
print("large", len(large_run.stdout) == len(large), len(large_run.stderr) == 0)
