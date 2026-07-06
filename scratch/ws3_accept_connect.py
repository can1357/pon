import socket
import threading

server = socket.socket()
server.bind(("127.0.0.1", 0))
server.listen(1)
port = server.getsockname()[1]
seen = []


def worker():
    conn, _addr = server.accept()
    data = conn.recv(4)
    conn.sendall(data)
    conn.close()
    server.close()
    seen.append(data)


t = threading.Thread(target=worker)
t.start()
client = socket.create_connection(("127.0.0.1", port), timeout=2)
client.sendall(b"ping")
print(client.recv(4).decode())
client.close()
t.join(2)
print("joined", not t.is_alive(), len(seen))
