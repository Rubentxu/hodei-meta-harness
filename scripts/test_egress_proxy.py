import contextlib
import importlib.util
import io
import ipaddress
import pathlib
import socket
import threading
import time
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location("egress_proxy", pathlib.Path(__file__).with_name("egress-proxy.py"))
proxy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(proxy)


def answer(ip, family=socket.AF_INET):
    address = (ip, 443) if family == socket.AF_INET else (ip, 443, 0, 0)
    return (family, socket.SOCK_STREAM, socket.IPPROTO_TCP, "", address)


class PolicyTests(unittest.TestCase):
    def test_networks_are_not_string_prefixes(self):
        network = proxy.client_network("172.17.")
        self.assertIn(ipaddress.ip_address("172.17.5.1"), network)
        self.assertNotIn(ipaddress.ip_address("172.170.5.1"), network)
        for invalid in ("172.1", "172.17.0.1/16", "", "172.17.evil"):
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                proxy.client_network(invalid)

    def test_a_client_network_too_broad_is_refused(self):
        # A /0 would make the proxy an open relay for the whole host.
        for too_broad in ("0.0.0.0/0", "0.", "0/0", "10.0.0.0/8",
                          "192.168.0.0/8", "172.16.0.0/11"):
            with self.subTest(value=too_broad), self.assertRaises(ValueError):
                proxy.client_network(too_broad)
        # The pools Docker allocates the sandbox networks from are all
        # inside the default client network.
        default = proxy.client_network(proxy.DEFAULT_ALLOWED_PREFIXES[0])
        for pool in range(17, 32):
            with self.subTest(pool=pool):
                self.assertIn(ipaddress.ip_address("172.%d.0.2" % pool), default)

    def test_a_client_network_that_is_not_ipv4_is_refused(self):
        with self.assertRaises(ValueError):
            proxy.client_network("2001:db8::/32")

    def test_authority_bypasses_denied_before_dns(self):
        for value in ("github.com:80", "github.com:443:1", "github.com.evil:443",
                      "evilgithub.com:443", "github.com.:443", "user:secret@github.com:443",
                      "github.com@evil.com:443", "https://github.com:443", "127.0.0.1:443",
                      "[::1]:443", "github.com:0443", "github.com:443/path"):
            with self.subTest(value=value), mock.patch.object(proxy.socket, "getaddrinfo") as dns:
                with self.assertRaises(ValueError):
                    proxy.resolve_destination(value, proxy.DEFAULT_ALLOWED_DOMAINS)
                dns.assert_not_called()

    def test_all_dns_answers_must_be_global(self):
        for ip, family in (("127.0.0.1", socket.AF_INET), ("10.0.0.1", socket.AF_INET),
                           ("169.254.169.254", socket.AF_INET), ("224.0.0.1", socket.AF_INET),
                           ("::1", socket.AF_INET6), ("fc00::1", socket.AF_INET6),
                           ("fe80::1", socket.AF_INET6), ("::ffff:127.0.0.1", socket.AF_INET6)):
            with self.subTest(ip=ip), mock.patch.object(proxy.socket, "getaddrinfo", return_value=[answer("8.8.8.8"), answer(ip, family)]):
                with self.assertRaises(ValueError):
                    proxy.resolve_destination("github.com:443", proxy.DEFAULT_ALLOWED_DOMAINS)

    def test_connect_uses_validated_ipv6_without_resolution(self):
        resolved = [answer("2606:4700:4700::1111", socket.AF_INET6)]
        with mock.patch.object(proxy.socket, "getaddrinfo", return_value=resolved) as dns:
            answers = proxy.resolve_destination("GITHUB.COM:443", proxy.DEFAULT_ALLOWED_DOMAINS)
        dns.assert_called_once()
        with mock.patch.object(proxy.socket, "socket") as factory, mock.patch.object(proxy.socket, "getaddrinfo") as dns:
            remote = proxy.connect_validated(answers)
            factory.assert_called_once_with(socket.AF_INET6, socket.SOCK_STREAM, socket.IPPROTO_TCP)
            remote.connect.assert_called_once_with(resolved[0][4])
            dns.assert_not_called()

    def test_empty_dns_and_scoped_ipv6_rejected(self):
        for answers in ([], [(socket.AF_INET6, socket.SOCK_STREAM, socket.IPPROTO_TCP, "", ("2606:4700:4700::1111", 443, 0, 1))]):
            with mock.patch.object(proxy.socket, "getaddrinfo", return_value=answers), self.assertRaises(ValueError):
                proxy.resolve_destination("github.com:443", proxy.DEFAULT_ALLOWED_DOMAINS)


class HTTPTests(unittest.TestCase):
    def test_allowed_connect_relays_bytes(self):
        server = proxy.RestrictedProxyServer(("127.0.0.1", 0), proxy.ProxyHandler)
        server.allowed_networks = (ipaddress.ip_network("127.0.0.0/8"),)
        server.allowed_domains = proxy.DEFAULT_ALLOWED_DOMAINS
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        upstream, peer = socket.socketpair()
        peer.settimeout(2)
        try:
            with mock.patch.object(proxy.socket, "getaddrinfo", return_value=[answer("8.8.8.8")]), mock.patch.object(proxy, "connect_validated", return_value=upstream):
                # Avoid create_connection: its own DNS lookup is mocked above.
                with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as client:
                    client.settimeout(2)
                    client.connect(server.server_address)
                    client.sendall(b"CONNECT github.com:443 HTTP/1.1\r\nHost: github.com\r\n\r\n")
                    self.assertIn(b" 200 ", client.recv(4096))
                    client.sendall(b"hello")
                    self.assertEqual(peer.recv(5), b"hello")
                    peer.sendall(b"world")
                    self.assertEqual(client.recv(5), b"world")
        finally:
            peer.close()
            server.shutdown()
            server.server_close()
            worker.join()

    def test_denied_requests_do_not_log_credentials(self):
        server = proxy.RestrictedProxyServer(("127.0.0.1", 0), proxy.ProxyHandler)
        server.allowed_networks = (ipaddress.ip_network("127.0.0.0/8"),)
        server.allowed_domains = proxy.DEFAULT_ALLOWED_DOMAINS
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        try:
            with contextlib.redirect_stderr(io.StringIO()) as logs:
                with socket.create_connection(server.server_address) as client:
                    client.sendall(b"CONNECT user:secret@github.com:443 HTTP/1.1\r\nHost: github.com\r\nProxy-Authorization: Basic c2VjcmV0\r\n\r\n")
                    self.assertIn(b" 403 ", client.recv(4096))
                self.assertNotIn("secret", logs.getvalue())
                self.assertNotIn("c2VjcmV0", logs.getvalue())
        finally:
            server.shutdown()
            server.server_close()
            worker.join()

    def test_unallowed_client_is_denied_before_resolution(self):
        handler = object.__new__(proxy.ProxyHandler)
        handler.client_address = ("172.170.1.1", 12)
        handler.server = type("Server", (), {"allowed_networks": (proxy.client_network("172.17."),)})()
        self.assertFalse(handler.client_allowed())

    def test_the_number_of_open_tunnels_is_bounded(self):
        # The clients are the sandboxes, and a sandbox is not trusted:
        # an unbounded number of tunnels would starve every other
        # sandbox of egress.
        server = proxy.RestrictedProxyServer(("127.0.0.1", 0), proxy.ProxyHandler)
        server.max_tunnels = 2
        try:
            self.assertTrue(server.acquire_tunnel())
            self.assertTrue(server.acquire_tunnel())
            self.assertFalse(server.acquire_tunnel())
            server.release_tunnel()
            self.assertTrue(server.acquire_tunnel())
        finally:
            server.server_close()

    def test_a_finished_tunnel_gives_its_slot_back(self):
        # A build opens many connections in sequence. If a closed tunnel
        # kept its slot, the proxy would refuse the next builds and the
        # lane would fail for a reason that has nothing to do with the
        # candidate.
        server = proxy.RestrictedProxyServer(("127.0.0.1", 0), proxy.ProxyHandler)
        server.max_tunnels = 1
        server.allowed_networks = (ipaddress.ip_network("127.0.0.0/8"),)
        server.allowed_domains = proxy.DEFAULT_ALLOWED_DOMAINS
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        try:
            for _ in range(3):
                upstream, peer = socket.socketpair()
                peer.settimeout(2)
                try:
                    with mock.patch.object(proxy.socket, "getaddrinfo", return_value=[answer("8.8.8.8")]), mock.patch.object(proxy, "connect_validated", return_value=upstream):
                        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as client:
                            client.settimeout(5)
                            client.connect(server.server_address)
                            client.sendall(b"CONNECT github.com:443 HTTP/1.1\r\nHost: github.com\r\n\r\n")
                            self.assertIn(b" 200 ", client.recv(4096))
                            client.sendall(b"hello")
                            self.assertEqual(peer.recv(5), b"hello")
                            peer.sendall(b"world")
                            self.assertEqual(client.recv(5), b"world")
                        # The upstream closes: the tunnel is over and its
                        # slot must be free again.
                        peer.close()
                        deadline = time.monotonic() + 5
                        while server.tunnels and time.monotonic() < deadline:
                            time.sleep(0.05)
                        self.assertEqual(server.tunnels, 0)
                finally:
                    peer.close()
                    upstream.close()
        finally:
            server.shutdown()
            server.server_close()
            worker.join()


if __name__ == "__main__":
    unittest.main()
