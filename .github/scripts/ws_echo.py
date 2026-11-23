#!/usr/bin/env python3
"""A tiny WebSocket echo server and client using only the standard library.

    ws_echo.py serve PORT            echo every text frame back, forever
    ws_echo.py send HOST PORT HOSTHDR PATH MESSAGE
                                     connect, send MESSAGE, print the echo

Used by CI to check WebSocket upgrades through nginx and proxlet.
"""

import base64
import hashlib
import os
import socket
import socketserver
import struct
import sys

GUID = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


def recv_exact(sock, n):
    buf = b""
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            raise ConnectionError("connection closed")
        buf += chunk
    return buf


def read_frame(sock):
    head = recv_exact(sock, 2)
    length = head[1] & 0x7F
    masked = head[1] & 0x80
    if length == 126:
        length = struct.unpack(">H", recv_exact(sock, 2))[0]
    elif length == 127:
        length = struct.unpack(">Q", recv_exact(sock, 8))[0]
    mask = recv_exact(sock, 4) if masked else b"\0\0\0\0"
    data = bytearray(recv_exact(sock, length))
    for i in range(length):
        data[i] ^= mask[i % 4]
    return head[0] & 0x0F, bytes(data)


def write_frame(sock, payload, mask):
    header = bytearray([0x81])
    flag = 0x80 if mask else 0
    if len(payload) < 126:
        header.append(flag | len(payload))
    else:
        header.append(flag | 126)
        header += struct.pack(">H", len(payload))
    if mask:
        key = os.urandom(4)
        header += key
        payload = bytes(b ^ key[i % 4] for i, b in enumerate(payload))
    sock.sendall(bytes(header) + payload)


def read_head(sock):
    buf = b""
    while b"\r\n\r\n" not in buf:
        chunk = sock.recv(1)
        if not chunk:
            raise ConnectionError("connection closed during handshake")
        buf += chunk
    return buf.decode()


class Echo(socketserver.BaseRequestHandler):
    def handle(self):
        head = read_head(self.request)
        key = next(
            line.split(":", 1)[1].strip()
            for line in head.split("\r\n")
            if line.lower().startswith("sec-websocket-key:")
        )
        accept = base64.b64encode(hashlib.sha1(key.encode() + GUID).digest()).decode()
        self.request.sendall(
            (
                "HTTP/1.1 101 Switching Protocols\r\n"
                "Upgrade: websocket\r\nConnection: Upgrade\r\n"
                f"Sec-WebSocket-Accept: {accept}\r\n\r\n"
            ).encode()
        )
        while True:
            try:
                opcode, data = read_frame(self.request)
            except ConnectionError:
                return  # the client hung up without a close frame
            if opcode == 8:
                return
            write_frame(self.request, data, mask=False)


def serve(port):
    socketserver.ThreadingTCPServer.allow_reuse_address = True
    with socketserver.ThreadingTCPServer(("127.0.0.1", port), Echo) as server:
        server.serve_forever()


def send(host, port, host_header, path, message):
    sock = socket.create_connection((host, port), timeout=10)
    key = base64.b64encode(os.urandom(16)).decode()
    sock.sendall(
        (
            f"GET {path} HTTP/1.1\r\nHost: {host_header}\r\n"
            "Upgrade: websocket\r\nConnection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        ).encode()
    )
    head = read_head(sock)
    status = head.split("\r\n", 1)[0]
    if " 101 " not in status:
        sys.exit(f"upgrade refused: {status}")
    write_frame(sock, message.encode(), mask=True)
    _, data = read_frame(sock)
    print(data.decode())


if __name__ == "__main__":
    if sys.argv[1] == "serve":
        serve(int(sys.argv[2]))
    else:
        send(sys.argv[2], int(sys.argv[3]), sys.argv[4], sys.argv[5], sys.argv[6])
