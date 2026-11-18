#!/usr/bin/env python3
"""Restricted egress proxy for the sandbox lane.

The sandboxes reach the internet only through this proxy. It speaks HTTP
CONNECT and accepts clients from the Docker subnets only. It binds to the
Docker bridge gateway and NOT to every interface: a CI runner with a public
address must never expose an open proxy.

    scripts/egress-proxy.py --bind 172.17.0.1 --port 8888

The proxy writes a "ready" line to stderr once it accepts connections, and
exits with a clear reason when it cannot bind.

This is lane infrastructure, not product code: it exists so the probes have
the restricted egress path their profiles require. Python 3 is the choice
because it is present on the CI runner and on the development host without
adding a toolchain.
"""

import argparse
import socket
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# The Docker subnets a sandbox may come from.
DEFAULT_ALLOWED_PREFIXES = ("172.17.", "172.18.", "172.19.", "172.20.")

# The address the probes use to reach the proxy (see DEFAULT_PROXY_URL in
# crates/hodei-mh/src/profile_run.rs).
DEFAULT_BIND = "172.17.0.1"
DEFAULT_PORT = 8888


def pump(src, dst):
    """Copy bytes both ways until one side closes."""
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

    def client_allowed(self):
        peer = self.client_address[0]
        return peer.startswith(self.server.allowed_prefixes)

    def do_CONNECT(self):
        if not self.client_allowed():
            self.send_response(403)
            self.end_headers()
            self.wfile.write(b"the client is not in an allowed subnet\n")
            return
        try:
            host, port = self.path.rsplit(":", 1)
            remote = socket.create_connection((host, int(port)), timeout=20)
        except (OSError, ValueError) as error:
            self.send_response(502)
            self.end_headers()
            self.wfile.write(str(error).encode())
            return
        self.send_response(200, "Connection Established")
        self.end_headers()
        upstream = threading.Thread(target=pump, args=(self.connection, remote), daemon=True)
        upstream.start()
        pump(remote, self.connection)
        remote.close()

    def log_message(self, fmt, *args):
        sys.stderr.write("proxy: " + fmt % args + "\n")
        sys.stderr.flush()


def parse_args():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bind", default=DEFAULT_BIND, help="address to bind")
    parser.add_argument("--port", type=int, default=DEFAULT_PORT, help="port to bind")
    parser.add_argument(
        "--allow-prefix",
        action="append",
        default=None,
        help="client subnet prefix allowed to use the proxy (repeatable)",
    )
    return parser.parse_args()


def main():
    args = parse_args()
    allowed = tuple(args.allow_prefix) if args.allow_prefix else DEFAULT_ALLOWED_PREFIXES
    try:
        server = ThreadingHTTPServer((args.bind, args.port), ProxyHandler)
    except OSError as error:
        sys.stderr.write(
            f"the restricted egress proxy could not bind {args.bind}:{args.port}: {error}\n"
        )
        sys.stderr.flush()
        return 1
    server.allowed_prefixes = allowed
    sys.stderr.write(f"connect proxy ready on {args.bind}:{args.port}\n")
    sys.stderr.flush()
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
