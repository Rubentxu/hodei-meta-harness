#!/usr/bin/env python3
"""HTTPS CONNECT proxy with explicit destination and client network policies.

--allow-domain adds an exact DNS name (repeatable); no wildcards are accepted.
--allow-prefix accepts CIDR networks or legacy whole-octet IPv4 prefixes such
as 172.17.  Neither option accepts arbitrary string prefixes.
"""

import argparse
import ipaddress
import re
import socket
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# The client networks the sandboxes come from. Docker allocates the
# per-sandbox networks from 172.17.0.0/16 to 172.31.0.0/16, so the whole
# 172.16.0.0/12 block is what a sandbox can have: a narrower list would
# refuse a sandbox that Docker placed in a later pool.
DEFAULT_ALLOWED_PREFIXES = ("172.16.0.0/12",)
DEFAULT_ALLOWED_DOMAINS = frozenset({
    "github.com", "api.github.com", "codeload.github.com", "objects.githubusercontent.com",
    "crates.io", "index.crates.io", "static.crates.io",
    "registry.npmjs.org", "registry.yarnpkg.com",
    "repo.maven.apache.org", "repo1.maven.org", "plugins.gradle.org",
    "services.gradle.org", "downloads.gradle.org",
    "api.anthropic.com", "api.openai.com",
})
DEFAULT_BIND = "172.17.0.1"
DEFAULT_PORT = 8888
# The narrowest client network the proxy accepts. A /0 would turn the
# proxy into an open relay for the whole host, and a /8 would cover
# sixteen million addresses. The pools a sandbox can come from live in
# 172.16.0.0/12, so that is the widest client network the proxy takes.
MIN_CLIENT_PREFIX_LENGTH = 12
# How many tunnels may be open at the same time. The clients are the
# sandboxes, and a sandbox is not trusted: an unbounded number of
# tunnels would starve every other sandbox of egress. The bound is
# generous because a real build opens many connections in parallel and
# refuses the work when the proxy says no.
MAX_TUNNELS = 512
DOMAIN_RE = re.compile(r"(?=.{1,253}\Z)(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\Z", re.ASCII)


def domain_name(value):
    value = value.lower()
    if not DOMAIN_RE.fullmatch(value):
        raise ValueError("invalid domain")
    try:
        ipaddress.ip_address(value)
    except ValueError:
        return value
    raise ValueError("IP literals are not allowed")


def client_network(value):
    if re.fullmatch(r"(?:[0-9]{1,3}\.){1,3}", value, re.ASCII):
        parts = value[:-1].split(".")
        value = ".".join(parts + ["0"] * (4 - len(parts))) + "/" + str(len(parts) * 8)
    network = ipaddress.ip_network(value, strict=True)
    # A client network that covers the whole address space would make
    # the proxy an open relay for every host that can reach it.
    if network.version != 4 or network.prefixlen < MIN_CLIENT_PREFIX_LENGTH:
        raise ValueError("a client network must be an IPv4 network of at least /%d"
                         % MIN_CLIENT_PREFIX_LENGTH)
    return network


def resolve_destination(authority, allowed_domains):
    # Only a DNS authority and canonical HTTPS port are accepted. This also
    # rejects userinfo, URL syntax, IPv6 literals, trailing dots and zones.
    host, sep, port = authority.partition(":")
    if not sep or port != "443":
        raise ValueError("invalid HTTPS destination")
    host = domain_name(host)
    if host not in allowed_domains:
        raise ValueError("destination not allowed")
    answers = socket.getaddrinfo(host, 443, type=socket.SOCK_STREAM, proto=socket.IPPROTO_TCP)
    if not answers:
        raise ValueError("empty DNS answer")
    for family, socktype, proto, _, address in answers:
        if family not in (socket.AF_INET, socket.AF_INET6) or socktype != socket.SOCK_STREAM:
            raise ValueError("unsupported DNS answer")
        ip = ipaddress.ip_address(address[0])
        # IPv4-mapped IPv6 must be evaluated as IPv4 as well.
        mapped = getattr(ip, "ipv4_mapped", None)
        if (not ip.is_global or ip.is_multicast or ip.is_reserved
                or (mapped is not None and (not mapped.is_global or mapped.is_multicast or mapped.is_reserved))
                or address[1] != 443
                or (family == socket.AF_INET and ip.version != 4)
                or (family == socket.AF_INET6 and ip.version != 6)):
            raise ValueError("non-global DNS answer")
        if family == socket.AF_INET6 and ("%" in address[0] or address[3] != 0):
            raise ValueError("scoped IPv6 answer")
    return answers


def connect_validated(answers):
    for family, socktype, proto, _, address in answers:
        remote = socket.socket(family, socktype, proto)
        try:
            remote.settimeout(20)
            remote.connect(address)
            remote.settimeout(None)
            return remote
        except OSError:
            remote.close()
    raise OSError("upstream unavailable")


def pump(src, dst):
    try:
        while True:
            data = src.recv(65536)
            if not data:
                break
            dst.sendall(data)
    except OSError:
        pass
    finally:
        try:
            dst.shutdown(socket.SHUT_WR)
        except OSError:
            pass


class ProxyHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    # Avoid buffering tunnel bytes beyond the CONNECT headers.
    rbufsize = 0

    def client_allowed(self):
        try:
            peer = ipaddress.ip_address(self.client_address[0])
            if getattr(peer, "ipv4_mapped", None) is not None:
                peer = peer.ipv4_mapped
            return any(peer.version == network.version and peer in network for network in self.server.allowed_networks)
        except ValueError:
            return False

    def reject(self, status):
        self.send_response(status)
        self.send_header("Content-Length", "0")
        self.send_header("Connection", "close")
        self.end_headers()
        self.close_connection = True

    def do_CONNECT(self):
        if not self.client_allowed():
            self.reject(403)
            return
        try:
            answers = resolve_destination(self.path, self.server.allowed_domains)
        except ValueError:
            self.reject(403)
            return
        except OSError:
            self.reject(502)
            return
        try:
            remote = connect_validated(answers)
        except OSError:
            self.reject(502)
            return
        # The tunnel is accounted for: a sandbox that opens tunnels
        # without limit would take the egress of every other sandbox
        # with it.
        if not self.server.acquire_tunnel():
            remote.close()
            self.reject(503)
            return
        released = False
        try:
            self.send_response(200, "Connection Established")
            self.end_headers()
            self.close_connection = True
            upstream = threading.Thread(target=pump, args=(self.connection, remote), daemon=True)
            upstream.start()
            try:
                # The upstream end of the tunnel. When it closes, the
                # work is done and the slot belongs back to the pool:
                # a client that keeps the socket open must not hold a
                # slot that the next build needs.
                pump(remote, self.connection)
            finally:
                self.server.release_tunnel()
                released = True
                remote.close()
            upstream.join(timeout=20)
        finally:
            if not released:
                self.server.release_tunnel()

    def log_message(self, fmt, *args):
        # BaseHTTPRequestHandler logs the entire request target, which may
        # contain userinfo or other credentials, even on malformed requests.
        pass


class RestrictedProxyServer(ThreadingHTTPServer):
    """The proxy server: it accounts for the tunnels it has open.

    The clients are the sandboxes, and a sandbox is not trusted. A
    number of tunnels without limit would take the egress of every
    other sandbox with it.
    """

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.tunnel_lock = threading.Lock()
        self.tunnels = 0
        self.max_tunnels = MAX_TUNNELS

    def acquire_tunnel(self):
        with self.tunnel_lock:
            if self.tunnels >= self.max_tunnels:
                return False
            self.tunnels += 1
            return True

    def release_tunnel(self):
        with self.tunnel_lock:
            self.tunnels = max(0, self.tunnels - 1)


def parse_args():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bind", default=DEFAULT_BIND, help="address to bind")
    parser.add_argument("--port", type=int, default=DEFAULT_PORT, help="port to bind")
    parser.add_argument("--allow-prefix", action="append", default=None, help="client CIDR or whole-octet IPv4 prefix (repeatable)")
    parser.add_argument("--allow-domain", action="append", default=[], help="additional exact HTTPS domain (repeatable)")
    args = parser.parse_args()
    try:
        args.allowed_networks = tuple(client_network(value) for value in (args.allow_prefix or DEFAULT_ALLOWED_PREFIXES))
        args.allowed_domains = DEFAULT_ALLOWED_DOMAINS | frozenset(domain_name(value) for value in args.allow_domain)
    except ValueError:
        parser.error("invalid client network or domain")
    return args


def main():
    args = parse_args()
    try:
        server = RestrictedProxyServer((args.bind, args.port), ProxyHandler)
    except OSError as error:
        sys.stderr.write(f"the restricted egress proxy could not bind: {error}\n")
        return 1
    server.allowed_networks = args.allowed_networks
    server.allowed_domains = args.allowed_domains
    sys.stderr.write(f"connect proxy ready on {args.bind}:{args.port}\n")
    sys.stderr.flush()
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
