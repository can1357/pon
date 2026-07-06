import socket
import threading

received = []
ready = threading.Event()
port_box = []

def server():
    srv = socket.socket()
    srv.bind(("127.0.0.1", 0))
    srv.listen(1)
    port_box.append(srv.getsockname()[1])
    ready.set()
    conn, _ = srv.accept()
    received.append(conn.recv(4).decode())
    conn.sendall(b"pong")
    conn.close()
    srv.close()

thread = threading.Thread(target=server)
thread.start()
ready.wait()
client = socket.create_connection(("127.0.0.1", port_box[0]))
client.sendall(b"ping")
print(client.recv(4).decode())
client.close()
thread.join()
print(received[0])
