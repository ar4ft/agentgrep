#!/usr/bin/env python3
"""Exercise the Ollama adapter with a local fixture; optionally test real LiteParse.

Uses loopback port 11434; fails if another service already owns it. Does not
download models, change Ollama configuration, or send source to an external host.
"""
import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import subprocess
import tempfile
import threading


class OllamaFixture(BaseHTTPRequestHandler):
    calls = []

    def log_message(self, *_args):
        pass

    def do_POST(self):
        if self.headers.get("Transfer-Encoding") == "chunked":
            body = b""
            while True:
                length = int(self.rfile.readline().strip(), 16)
                if not length:
                    self.rfile.readline()
                    break
                body += self.rfile.read(length)
                self.rfile.read(2)
        else:
            body = self.rfile.read(int(self.headers["Content-Length"]))
        request = json.loads(body)
        self.calls.append(request)
        assert self.path == "/api/embed"
        if request["model"] == "missing-model":
            self.send_response(404)
            self.end_headers()
            return
        if request["model"] == "broken-vectors":
            vectors = []
        else:
            vectors = [[1.0, 0.0] if "throttle" in s or "too quickly" in s else [0.0, 1.0] for s in request["input"]]
        response = json.dumps({"embeddings": vectors}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(response)))
        self.end_headers()
        self.wfile.write(response)


def invoke(binary, args, good=True):
    result = subprocess.run([binary] + args, capture_output=True, text=True, timeout=90)
    if good:
        assert result.returncode == 0, result.stderr
        return json.loads(result.stdout)
    assert result.returncode == 2 and not result.stdout, result
    return json.loads(result.stderr)


def make_pdf(path):
    stream = b"BT /F1 16 Tf 50 730 Td (Retry policy uses exponential backoff.) Tj ET"
    objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>",
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n" + stream + b"\nendstream",
    ]
    data = b"%PDF-1.4\n"
    offsets = []
    for n, obj in enumerate(objects, 1):
        offsets.append(len(data))
        data += f"{n} 0 obj\n".encode() + obj + b"\nendobj\n"
    xref = len(data)
    data += b"xref\n0 6\n0000000000 65535 f \n"
    for offset in offsets:
        data += f"{offset:010} 00000 n \n".encode()
    data += b"trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n" + str(xref).encode() + b"\n%%EOF\n"
    path.write_bytes(data)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default="target/release/agx")
    parser.add_argument("--liteparse", action="store_true", help="Also run a real PDF through an already installed lit CLI")
    args = parser.parse_args()
    binary = str(Path(args.binary).resolve(strict=True))
    server = ThreadingHTTPServer(("127.0.0.1", 11434), OllamaFixture)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix="agx-adapter-") as directory:
            root = Path(directory)
            source = root / "requests.py"
            source.write_text("def throttle(request):\n    return request\n")
            (root / "paint.py").write_text("def canvas(value):\n    return value\n")
            command = ["search", "requests arriving too quickly", directory, "--mode", "hybrid", "--model", "fixture"]
            first = invoke(binary, command)
            assert first["results"][0]["path"] == "requests.py", first
            calls = len(OllamaFixture.calls)
            second = invoke(binary, command)
            assert first["results"] == second["results"]
            assert len(OllamaFixture.calls) == calls + 1, "Warm query must reuse cached chunk vectors"
            source.write_text("def throttle(request):\n    return request + 1\n")
            invoke(binary, command)
            assert len(OllamaFixture.calls) == calls + 3, "Changed chunks must be embedded before the query"
            for model in ["missing-model", "broken-vectors"]:
                invoke(binary, command[:-1] + [model], good=False)
            if args.liteparse:
                pdf = root / "document with spaces.pdf"
                markdown = root / "extracted.md"
                make_pdf(pdf)
                result = invoke(binary, ["parse", str(pdf), "--out", str(markdown)])
                assert result["source"] == str(pdf.resolve())
                assert "exponential backoff" in markdown.read_text().lower()
                result = invoke(binary, ["search", "exponential backoff", directory, "-F"])
                assert result["results"][0]["path"] == "extracted.md"
                invoke(binary, ["parse", str(pdf), "--out", str(markdown)], good=False)
            invoke(binary, ["clean", directory])
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
    print(json.dumps({"mock_ollama": "passed", "real_liteparse": "passed" if args.liteparse else "not requested", "scope": "Adapter correctness, not semantic retrieval quality"}))


if __name__ == "__main__":
    main()
