#!/usr/bin/env python3
"""Submit a 10-correct / 5-fake commit-reveal round for the graph UI.

This is a protocol and UI demo. It represents builders with distinct IDs but
does not execute Nix builds or start builder-node daemons.
"""

import argparse
import base64
import collections
import hashlib
import json
import os
import urllib.error
import urllib.request
from datetime import datetime, timezone


COMMITMENT_DOMAIN = b"reproductive-nix-cache/evidence/v1\0"


def parse_args():
    parser = argparse.ArgumentParser(
        description="Run a commit-reveal graph demo with matching and fake results."
    )
    parser.add_argument("--server", default="http://127.0.0.1:5123")
    parser.add_argument("--correct-builders", type=int, default=10)
    parser.add_argument("--fake-builders", type=int, default=5)
    parser.add_argument(
        "--derivation-path",
        default="/nix/store/demo15-reproductive-nix-cache.drv",
    )
    parser.add_argument(
        "--store-path",
        default="/nix/store/demo15-reproductive-nix-cache",
    )
    parser.add_argument(
        "--correct-hash",
        default="sha256-correct-result-10-builders",
    )
    parser.add_argument(
        "--fake-hash",
        default="sha256-fake-result-5-builders",
    )
    parser.add_argument(
        "--correct-cache",
        default="https://cache-correct.demo.invalid/builds",
    )
    parser.add_argument(
        "--fake-cache",
        default="https://cache-fake.demo.invalid/builds",
    )
    args = parser.parse_args()
    if args.correct_builders < 1 or args.fake_builders < 1:
        parser.error("builder counts must both be at least 1")
    args.server = args.server.rstrip("/")
    return args


def request(server, method, path, payload=None):
    body = None if payload is None else json.dumps(payload).encode()
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
        message = error.read().decode(errors="replace")
        raise RuntimeError(f"{method} {path}: HTTP {error.code}: {message}") from error


def make_evidence(args, builder_id, nar_hash, built_at):
    return {
        "schema_version": 4,
        "builder_id": builder_id,
        "package": {"repository": "demo", "name": "fifteen-builders"},
        "claims": [
            {
                "type": "build",
                "payload": {
                    "source": {
                        "resolved_url": "demo:15-builders",
                        "revision": "demo-revision",
                        "nar_hash": "sha256-demo-source",
                    },
                    "derivation_path": args.derivation_path,
                    "build_statement": {
                        "outputs": [
                            {
                                "output_name": "out",
                                "output_store_path": args.store_path,
                                "nar_hash": nar_hash,
                                "nar_size": 1_515_000,
                                "references": [],
                                "closure_root": args.store_path,
                                "content_addressed": None,
                            }
                        ],
                        "build_log_digest": None,
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


def run(args):
    correct_ids = [
        f"builder-{index:02d}" for index in range(1, args.correct_builders + 1)
    ]
    fake_ids = [
        f"builder-{index:02d}"
        for index in range(
            args.correct_builders + 1,
            args.correct_builders + args.fake_builders + 1,
        )
    ]
    builders = [
        *((builder_id, args.correct_hash, "correct") for builder_id in correct_ids),
        *((builder_id, args.fake_hash, "fake") for builder_id in fake_ids),
    ]
    total = len(builders)
    built_at = datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")
    submissions = []
    round_id = None

    print("COMMIT PHASE")
    for builder_id, nar_hash, kind in builders:
        evidence = make_evidence(args, builder_id, nar_hash, built_at)
        nonce, digest = make_commitment(evidence)
        try:
            status, receipt = request(
                args.server,
                "POST",
                "/v1/evidence/commitments",
                {
                    "builder_id": builder_id,
                    "derivation_path": args.derivation_path,
                    "digest": digest,
                },
            )
        except RuntimeError as error:
            raise RuntimeError(
                f"{error}\nStart the server with --commit-min-builders {total}."
            ) from error
        current_round_id = receipt["round"]["id"]
        round_id = current_round_id if round_id is None else round_id
        if current_round_id != round_id:
            raise RuntimeError("builders were assigned to different rounds")
        print(
            f"  {builder_id}: HTTP {status}, {kind}, "
            f"commits={receipt['round']['commit_count']}/{total}, "
            f"phase={receipt['round']['phase']}"
        )
        submissions.append((builder_id, nonce, evidence, kind))

    print("REVEAL PHASE")
    for builder_id, nonce, evidence, kind in submissions:
        cache_uri = args.correct_cache if kind == "correct" else args.fake_cache
        status, receipt = request(
            args.server,
            "POST",
            "/v1/evidence/reveals",
            {
                "round_id": round_id,
                "nonce": nonce,
                "evidence": evidence,
                "cache_locations": [{"uri": cache_uri}],
            },
        )
        print(
            f"  {builder_id}: HTTP {status}, {kind}, "
            f"evidence_id={receipt['evidence']['id']}"
        )

    _, round_status = request(
        args.server, "GET", f"/v1/evidence/rounds/{round_id}"
    )
    _, overview = request(args.server, "GET", "/v1/overview")
    overview_round = next(item for item in overview["rounds"] if item["id"] == round_id)
    groups = collections.Counter(
        participant["outputs"][0]["nar_hash"]
        for participant in overview_round["participants"]
    )

    print("FINAL")
    print(json.dumps(round_status, indent=2))
    for nar_hash, count in groups.most_common():
        print(f"  {count} builders -> {nar_hash}")


if __name__ == "__main__":
    run(parse_args())
