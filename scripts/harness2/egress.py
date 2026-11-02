"""Opaque TLS CONNECT relay; only the configured provider authority is accepted."""
from contextlib import contextmanager
import select
import socket
import socketserver
import threading


class Relay(socketserver.StreamRequestHandler):
    def handle(self):
        line=self.rfile.readline(8193)
        if len(line)>8192:
            return
        parts=line.decode("ascii",errors="replace").split()
        authority=self.server.authority
        if len(parts)!=3 or parts[0]!="CONNECT" or parts[1].lower()!=authority:
            self.wfile.write(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
            return
        total=len(line)
        while True:
            line=self.rfile.readline(8193)
            total+=len(line)
            if total>16384 or not line:
                return
            if line in (b"\r\n",b"\n"):
                break
        try:
            remote=socket.create_connection((authority[:-4],443),timeout=10)
        except OSError:
            self.wfile.write(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
            return
        try:
            self.wfile.write(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            remote.settimeout(None)
            peers=(self.connection,remote)
            while True:
                readable,_,_=select.select(peers,[],[],1)
                if self.server.stopping.is_set():
                    return
                for source in readable:
                    data=source.recv(65536)
                    if not data:
                        return
                    (remote if source is self.connection else self.connection).sendall(data)
        except OSError:
            pass
        finally:
            remote.close()


class Server(socketserver.ThreadingTCPServer):
    daemon_threads=True

    def handle_error(self,request,address):
        # Transport errors have no HTTP headers or credentials in diagnostics.
        pass


@contextmanager
def relay(host):
    with Server(("127.0.0.1",0),Relay) as server:
        server.authority=host+":443"
        server.stopping=threading.Event()
        thread=threading.Thread(target=server.serve_forever,daemon=True)
        thread.start()
        try:
            yield server.server_address[1]
        finally:
            server.stopping.set()
            server.shutdown()
            thread.join()
