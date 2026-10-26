#!/usr/bin/env python3
"""Minimal threaded static upstream for benchmark runs.

Returns a fixed body and never logs. Not a substitute for a production backend:
Python overhead can cap throughput, so record it as part of the methodology.
"""

import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

BODY = b"runnelon-bench-upstream\n"


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self) -> None:  # noqa: N802 (stdlib naming)
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(BODY)))
        self.end_headers()
        self.wfile.write(BODY)

    def log_message(self, *_args) -> None:
        pass


def main() -> None:
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 18000
    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    server.serve_forever()


if __name__ == "__main__":
    main()
