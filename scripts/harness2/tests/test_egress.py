from pathlib import Path
import socket
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0,str(Path(__file__).resolve().parents[1]))
from egress import relay


class EgressTests(unittest.TestCase):
    def test_foreign_authority_port_and_plain_http_never_connect(self):
        with relay("api.deepseek.com") as port, patch("egress.socket.create_connection") as upstream:
            for request in (b"CONNECT example.com:443 HTTP/1.1\r\n\r\n",b"CONNECT api.deepseek.com:80 HTTP/1.1\r\n\r\n",b"GET https://api.deepseek.com/ HTTP/1.1\r\n\r\n"):
                with socket.socket() as client:
                    client.connect(("127.0.0.1",port))
                    client.sendall(request)
                    self.assertIn(b"403",client.recv(4096))
            upstream.assert_not_called()

    def test_allowed_authority_is_the_only_connection_target(self):
        with relay("api.deepseek.com") as port, patch("egress.socket.create_connection",side_effect=OSError("synthetic outage")) as upstream:
            with socket.socket() as client:
                client.connect(("127.0.0.1",port))
                client.sendall(b"CONNECT api.deepseek.com:443 HTTP/1.1\r\n\r\n")
                self.assertIn(b"502",client.recv(4096))
            upstream.assert_called_once_with(("api.deepseek.com",443),timeout=10)
