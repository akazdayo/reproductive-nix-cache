#!/usr/bin/env python3
"""HTTP Builder Node that deliberately reveals forged build Evidence."""

import argparse
import base64
import hashlib
import json
import os
import re
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


COMMITMENT_DOMAIN = b"reproductive-nix-cache/evidence/v1\0"
FAKE_NAR_HASH = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
FAKE_CACHE_URI = "https://liar-cache.demo.invalid/builds"


def parse_args():
    parser = argparse.ArgumentParser()
    parser.add_argument("--listen", default="127.0.0.1:52345")
    parser.add_argument("--server", required=True)
    parser.add_argument("--builder-id", required=True)
    parser.add_argument("--derivation-path", required=True)
    args = parser.parse_args()
    host, separator, port = args.listen.rpartition(":")
    if not separator or not host:
        parser.error("--listen must use HOST:PORT syntax")
    args.listen_address = (host, int(port))
    args.server = args.server.rstrip("/")
    return args


def request(server, method, path, payload=None):
    body = None if payload is None else json.dumps(payload).encode()
    deadline = time.monotonic() + 60
    delay = 0.25
    while True:
        request = urllib.request.Request(
            server + path,
            data=body,
            method=method,
            headers={"Content-Type": "application/json", "Accept": "application/json"},
        )
        try:
            with urllib.request.urlopen(request, timeout=10) as response:
                return response.status, json.load(response)
        except urllib.error.HTTPError as error:
            if error.code < 500 or time.monotonic() >= deadline:
                message = error.read().decode(errors="replace")
                raise RuntimeError(
                    f"{method} {path}: HTTP {error.code}: {message}"
                ) from error
        except urllib.error.URLError as error:
            if time.monotonic() >= deadline:
                raise RuntimeError(f"{method} {path}: {error}") from error
        time.sleep(delay)
        delay = min(delay * 2, 2)


def make_evidence(args, package_ref):
    repository, package_name = package_ref.split("#", 1)
    safe_name = re.sub(r"[^a-zA-Z0-9._+-]", "-", package_name)
    fake_store_path = f"/nix/store/{'0' * 32}-liar-{safe_name}"
    built_at = datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")
    return {
        "schema_version": 4,
        "builder_id": args.builder_id,
        "package": {"repository": repository, "name": package_name},
        "claims": [
            {
                "type": "build",
                "payload": {
                    "source": {
                        "resolved_url": f"liar:{package_ref}",
                        "revision": "forged-revision",
                        "nar_hash": "sha256-forged-source",
                    },
                    "derivation_path": args.derivation_path,
                    "build_statement": {
                        "outputs": [
                            {
                                "output_name": "out",
                                "output_store_path": fake_store_path,
                                "nar_hash": FAKE_NAR_HASH,
                                "nar_size": 1,
                                "references": [],
                                "closure_root": fake_store_path,
                                "content_addressed": None,
                            }
                        ],
                        "build_log_digest": "sha256-forged-build-log",
                        "sbom_digest": None,
                        "test_result_digest": None,
                    },
                    "built_at": built_at,
                },
            }
        ],
    }


def make_commitment(evidence):
    nonce_bytes = os.urandom(32)
    canonical = json.dumps(
        evidence, sort_keys=True, separators=(",", ":"), ensure_ascii=False
    ).encode()
    digest = hashlib.sha256(COMMITMENT_DOMAIN + nonce_bytes + canonical).digest()
    nonce = base64.urlsafe_b64encode(nonce_bytes).rstrip(b"=").decode()
    encoded_digest = base64.urlsafe_b64encode(digest).rstrip(b"=").decode()
    return nonce, "sha256:" + encoded_digest


def forge_build(args, command):
    package_ref = command.get("package_ref")
    if not isinstance(package_ref, str) or "#" not in package_ref:
        raise ValueError("package_ref must use repository#attribute syntax")
    if command.get("substitute") is not True:
        raise ValueError("liar demo expects substitute=true")
    print(
        f"{args.builder_id}: received build request, "
        f"package_ref={package_ref}, substitute=true",
        flush=True,
    )

    evidence = make_evidence(args, package_ref)
    nonce, digest = make_commitment(evidence)
    status, receipt = request(
        args.server,
        "POST",
        "/v1/evidence/commitments",
        {
            "builder_id": args.builder_id,
            "derivation_path": args.derivation_path,
            "digest": digest,
        },
    )
    round_id = receipt["round"]["id"]
    print(
        f"{args.builder_id}: forged commitment accepted, HTTP {status}, "
        f"round #{round_id}",
        flush=True,
    )

    while True:
        _, round_status = request(
            args.server, "GET", f"/v1/evidence/rounds/{round_id}"
        )
        if round_status["phase"] == "revealing":
            break
        if round_status["phase"] in {"completed", "expired"}:
            raise RuntimeError(
                f"round #{round_id} closed before liar reveal: {round_status['phase']}"
            )
        time.sleep(0.25)

    status, reveal = request(
        args.server,
        "POST",
        "/v1/evidence/reveals",
        {
            "round_id": round_id,
            "nonce": nonce,
            "evidence": evidence,
            "cache_locations": [{"uri": FAKE_CACHE_URI}],
        },
    )
    evidence_id = reveal["evidence"]["id"]
    print(
        f"{args.builder_id}: LIED, HTTP {status}, evidence_id={evidence_id}, "
        f"hash={FAKE_NAR_HASH}, cache={FAKE_CACHE_URI}",
        flush=True,
    )
    return {
        "builder_id": args.builder_id,
        "round_id": round_id,
        "evidence_id": evidence_id,
    }


def handler(args):
    class LiarHandler(BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path != "/":
                self.send_error(404)
                return
            self.respond(200, b"ok", "text/plain")

        def do_POST(self):
            if self.path != "/v1/builds":
                self.send_error(404)
                return
            try:
                length = int(self.headers.get("Content-Length", "0"))
                command = json.loads(self.rfile.read(length))
                receipt = forge_build(args, command)
                self.respond(200, json.dumps(receipt).encode(), "application/json")
            except Exception as error:
                print(f"{args.builder_id}: liar build failed: {error}", flush=True)
                self.respond(
                    500,
                    json.dumps({"error": "liar build failed; see liar logs"}).encode(),
                    "application/json",
                )

        def respond(self, status, body, content_type):
            self.send_response(status)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, format_, *values):
            return

    return LiarHandler


def run(args):
    server = ThreadingHTTPServer(args.listen_address, handler(args))
    print(
        f"liar Builder Node {args.builder_id} listening on "
        f"{args.listen_address[0]}:{args.listen_address[1]}",
        flush=True,
    )
    server.serve_forever()


if __name__ == "__main__":
    run(parse_args())
