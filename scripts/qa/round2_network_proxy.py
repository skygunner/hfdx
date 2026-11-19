# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
# How to run: python proxy.py
"""Local CONNECT proxy for the round2 wire proof; never logs credentials."""
import base64
import json
import select
import socket
import socketserver
import sys
import threading


class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        self.connection.settimeout(10)
        first = self.rfile.readline(8192)
        if not first.startswith(b"CONNECT "):
            print(json.dumps({"event": "initial", "first_hex": first[:8].hex()}), flush=True)
            return
        parts = first.decode("ascii").strip().split()
        headers = {}
        while True:
            line = self.rfile.readline(8192)
            if line in (b"\r\n", b"\n", b""):
                break
            key, value = line.decode("ascii").split(":", 1)
            headers[key.lower()] = value.strip()
        authorized = headers.get("proxy-authorization") == "Basic " + base64.b64encode(b"proof:round2").decode()
        target = parts[1]
        allowed = target == "huggingface.co:443"
        print(json.dumps({"event": "connect", "target": target, "authorized": authorized, "allowed": allowed}), flush=True)
        if not authorized or not allowed:
            self.wfile.write(b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            return
        with socket.create_connection(("huggingface.co", 443), timeout=10) as upstream:
            self.wfile.write(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            upstream.setblocking(False)
            self.connection.setblocking(False)
            transferred = [0, 0]
            sockets = [self.connection, upstream]
            while True:
                readable, _, _ = select.select(sockets, [], [], 20)
                if not readable:
                    break
                for source in readable:
                    chunk = source.recv(65536)
                    if not chunk:
                        print(json.dumps({"event": "tunnel_closed", "client_bytes": transferred[0], "origin_bytes": transferred[1]}), flush=True)
                        return
                    target_socket = upstream if source is self.connection else self.connection
                    target_socket.setblocking(True)
                    target_socket.sendall(chunk)
                    target_socket.setblocking(False)
                    transferred[0 if source is self.connection else 1] += len(chunk)


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


with Server(("127.0.0.1", 0), Handler) as server:
    print(json.dumps({"event": "ready", "port": server.server_address[1]}), flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print(json.dumps({"event": "shutdown"}), flush=True)
