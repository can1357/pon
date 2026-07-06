import io

buf = io.BytesIO()
txt = io.TextIOWrapper(buf, "latin-1", None, "\r\n", False, True)
txt.write("é\n")
print(list(buf.getvalue()))
txt.reconfigure(encoding="ascii", errors="ignore", newline="\n", write_through=True)
txt.write("éx\n")
print(list(buf.getvalue()))
