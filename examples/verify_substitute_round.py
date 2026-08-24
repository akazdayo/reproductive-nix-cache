#!/usr/bin/env python3
"""Wait for and verify the seven-Builder substitute-enabled E2E round."""

import argparse
import json
import pathlib
import time
import urllib.error
import urllib.request


def parse_args():
    parser = argparse.ArgumentParser()
    parser.add_argument("--server", required=True)
    parser.add_argument("--manager-log", type=pathlib.Path, required=True)
    parser.add_argument("--package", required=True)
    parser.add_argument("--builders", type=int, required=True)
    parser.add_argument("--timeout-seconds", type=int, default=900)
    return parser.parse_args()


def request_json(url):
    request = urllib.request.Request(url, headers={"Accept": "application/json"})
    with urllib.request.urlopen(request, timeout=2) as response:
        return json.load(response)


def completed_dispatch(log, builders):
    completion_lines = [
        line for line in log.splitlines() if "build dispatch finished" in line
    ]
    for line in completion_lines:
        if f"succeeded={builders}" in line and "failed=0" in line:
            return True
        if "failed=0" not in line:
            raise RuntimeError(f"Builder dispatch failed: {line.strip()}")
    return False


def verify_round(args, overview):
    if not overview["rounds"]:
        return "waiting for Builders to create a round"

    round_ = overview["rounds"][0]
    phase = round_["phase"]
    commits = round_["commit_count"]
    reveals = round_["reveal_count"]
    status = f"round #{round_['id']}: {phase}, commits={commits}, reveals={reveals}"
    if phase == "expired":
        raise RuntimeError(status)
    if phase != "completed":
        return status
    if commits != args.builders or reveals != args.builders:
        raise RuntimeError(f"unexpected completed round counts: {status}")

    participants = round_["participants"]
    expected_ids = {f"substitute-{index:02d}" for index in range(1, args.builders + 1)}
    actual_ids = {participant["builder_id"] for participant in participants}
    if actual_ids != expected_ids:
        raise RuntimeError(f"unexpected Builders: {sorted(actual_ids)}")
    if any(participant["reveal_status"] != "success" for participant in participants):
        raise RuntimeError("a Builder did not reveal successfully")

    repository, package_name = args.package.split("#", 1)
    fingerprints = set()
    for participant in participants:
        if participant["package"] != {
            "repository": repository,
            "name": package_name,
        }:
            raise RuntimeError(
                f"unexpected package from {participant['builder_id']}: "
                f"{participant['package']}"
            )
        if not participant["outputs"]:
            raise RuntimeError(f"no outputs from {participant['builder_id']}")
        fingerprints.add(
            json.dumps(participant["outputs"], sort_keys=True, separators=(",", ":"))
        )
    if len(fingerprints) != 1:
        raise RuntimeError(f"Builders produced {len(fingerprints)} output variants")

    log = args.manager_log.read_text(errors="replace")
    dispatch_lines = [
        line for line in log.splitlines() if "build dispatch started" in line
    ]
    if not any("substitute=true" in line for line in dispatch_lines):
        return f"{status}; waiting for substitute=true Manager log"
    if not completed_dispatch(log, args.builders):
        return f"{status}; waiting for Round Manager completion"

    output = participants[0]["outputs"][0]
    print(
        f"E2E PASS: {args.builders} Builders agreed with substitute=true\n"
        f"round: #{round_['id']}\n"
        f"store path: {output['store_path']}\n"
        f"NAR hash: {output['nar_hash']}"
    )
    return None


def run(args):
    deadline = time.monotonic() + args.timeout_seconds
    previous_status = None
    while time.monotonic() < deadline:
        try:
            overview = request_json(args.server.rstrip("/") + "/v1/overview")
            status = verify_round(args, overview)
        except (urllib.error.URLError, TimeoutError) as error:
            status = f"waiting for Overview API: {error}"
        if status is None:
            return
        if status != previous_status:
            print(status, flush=True)
            previous_status = status
        time.sleep(0.5)
    raise RuntimeError(f"timed out after {args.timeout_seconds} seconds")


if __name__ == "__main__":
    try:
        run(parse_args())
    except Exception as error:
        raise SystemExit(f"E2E FAIL: {error}") from error
