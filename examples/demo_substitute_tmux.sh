#!/usr/bin/env bash
set -euo pipefail

SCRIPT_PATH="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"
REPOSITORY_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HONEST_BUILDER_COUNT=7
LIAR_COUNT=3
TOTAL_PARTICIPANT_COUNT=$((HONEST_BUILDER_COUNT + LIAR_COUNT))

write_result() {
    local message=$1
    printf '%s\n' "$message" >"${DEMO_RESULT}.tmp"
    mv "${DEMO_RESULT}.tmp" "$DEMO_RESULT"
}

run_manager() {
    local args=(
        --listen "0.0.0.0:${DEMO_MANAGER_PORT}"
        --database "$DEMO_DIR/demo.sqlite"
        --commit-min-builders "$DEMO_TOTAL_PARTICIPANT_COUNT"
        --cache-min-builders "$DEMO_HONEST_BUILDER_COUNT"
        --commit-window-seconds 600
        --reveal-window-seconds 600
    )
    local index
    for index in $(seq 1 "$DEMO_TOTAL_PARTICIPANT_COUNT"); do
        args+=(--builder-node "http://127.0.0.1:$((DEMO_MANAGER_PORT + index))")
    done

    cd "$DEMO_ROOT"
    exec > >(tee "$DEMO_DIR/manager.log") 2>&1
    exec env RUST_LOG=reproductive_nix_cache_server=debug \
        "$DEMO_ROOT/target/debug/reproductive-nix-cache-server" "${args[@]}"
}

run_liar() {
    local index=$1
    local port=$2
    local builder_id
    printf -v builder_id 'liar-%02d' "$index"

    exec > >(tee "$DEMO_DIR/${builder_id}.log") 2>&1
    exec python3 "$DEMO_ROOT/examples/demo_liar_builder.py" \
        --listen "127.0.0.1:${port}" \
        --server "$DEMO_MANAGER_URL" \
        --builder-id "$builder_id" \
        --derivation-path "$DEMO_DERIVATION_PATH"
}

run_builder() {
    local index=$1
    local port=$2
    local builder_id
    printf -v builder_id 'substitute-%02d' "$index"

    cd "$DEMO_ROOT"
    exec > >(tee "$DEMO_DIR/${builder_id}.log") 2>&1
    exec "$DEMO_ROOT/target/debug/reproductive-nix-cache-builder-node" \
        --listen "127.0.0.1:${port}" \
        --builder-id "$builder_id" \
        --server "$DEMO_MANAGER_HOST"
}

all_services_are_healthy() {
    curl -fsS "$DEMO_MANAGER_URL/healthz" >/dev/null 2>&1 || return 1
    local index
    for index in $(seq 1 "$DEMO_HONEST_BUILDER_COUNT"); do
        curl -fsS "http://127.0.0.1:$((DEMO_MANAGER_PORT + index))/" \
            >/dev/null 2>&1 || return 1
    done
    for index in $(seq 1 "$DEMO_LIAR_COUNT"); do
        curl -fsS \
            "http://127.0.0.1:$((DEMO_MANAGER_PORT + DEMO_HONEST_BUILDER_COUNT + index))/" \
            >/dev/null 2>&1 || return 1
    done
}

run_request() {
    printf 'Waiting for Round Manager, %s honest Builders, and %s liars' \
        "$DEMO_HONEST_BUILDER_COUNT" "$DEMO_LIAR_COUNT"
    for _ in $(seq 1 600); do
        if all_services_are_healthy; then
            printf ' ready\n\n'
            break
        fi
        printf '.'
        sleep 0.1
    done
    if ! all_services_are_healthy; then
        printf '\nServices did not become healthy.\n'
        write_result "FAIL: services did not become healthy"
        exit 1
    fi

    payload="$(python3 - "$DEMO_PACKAGE" <<'PY'
import json
import sys

print(json.dumps({"package_ref": sys.argv[1], "substitute": True, "claims": []}))
PY
)"
    printf 'REQUEST\n'
    printf 'curl -X POST %s/v1/builds %s\n' "$DEMO_MANAGER_URL" '\'
    printf "  -H 'Content-Type: application/json' %s\n" '\'
    printf "  -d '%s'\n\n" "$payload"

    response_file="$DEMO_DIR/queue-response.json"
    if ! http_status="$(curl -sS -o "$response_file" -w '%{http_code}' \
        -X POST "$DEMO_MANAGER_URL/v1/builds" \
        -H 'Content-Type: application/json' \
        -d "$payload")"; then
        write_result "FAIL: curl could not reach Round Manager"
        exit 1
    fi
    printf 'HTTP %s\n' "$http_status"
    python3 -m json.tool "$response_file"
    if [[ "$http_status" != 202 ]]; then
        write_result "FAIL: Round Manager returned HTTP ${http_status}"
        exit 1
    fi
    if ! python3 - "$response_file" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as response:
    receipt = json.load(response)
if receipt.get("queued") is not True or not isinstance(receipt.get("job_id"), int):
    raise SystemExit("invalid queue receipt")
PY
    then
        write_result "FAIL: invalid queue receipt"
        exit 1
    fi

    printf '\nThe request returned immediately. Waiting for seven real builds...\n'
    if python3 "$DEMO_ROOT/examples/verify_substitute_round.py" \
        --server "$DEMO_MANAGER_URL" \
        --manager-log "$DEMO_DIR/manager.log" \
        --package "$DEMO_PACKAGE" \
        --honest-builders "$DEMO_HONEST_BUILDER_COUNT" \
        --liars "$DEMO_LIAR_COUNT" \
        2>&1 | tee "$DEMO_DIR/verification.log"; then
        write_result "PASS: ${DEMO_HONEST_BUILDER_COUNT} honest Builders beat ${DEMO_LIAR_COUNT} liars"
        exit 0
    else
        status=${PIPESTATUS[0]}
        write_result "FAIL: substitute E2E exited with status ${status}"
        exit "$status"
    fi
}

run_summary() {
    printf 'SUBSTITUTE-ENABLED E2E DEMO\n\n'
    printf 'Graph UI:    %s/\n' "$DEMO_MANAGER_URL"
    printf 'Package:     %s\n' "$DEMO_PACKAGE"
    printf 'Honest:      %s real Builder Nodes\n' "$DEMO_HONEST_BUILDER_COUNT"
    printf 'Liars:       %s malicious Builder Nodes\n' "$DEMO_LIAR_COUNT"
    printf 'Substitute:  true\n'
    printf 'Session:     %s\n' "$DEMO_SESSION"
    printf 'Logs:        %s\n\n' "$DEMO_DIR"
    printf 'Window 0: control\n'
    printf 'Window 1: seven honest Builder Nodes\n'
    printf 'Window 2: three liars\n'
    printf 'Switch: Ctrl-b 0 / Ctrl-b 1 / Ctrl-b 2\n'
    printf 'Detach: Ctrl-b d\n'
    printf 'Stop:   tmux kill-session -t %s\n\n' "$DEMO_SESSION"
    printf 'Waiting for the E2E result...\n'
    while [[ ! -f "$DEMO_RESULT" ]]; do
        sleep 0.2
    done
    printf '\n'
    cat "$DEMO_RESULT"
}

case "${1:-}" in
    __manager)
        run_manager
        exit
        ;;
    __builder)
        run_builder "${2:?missing Builder index}" "${3:?missing Builder port}"
        exit
        ;;
    __liar)
        run_liar "${2:?missing liar index}" "${3:?missing liar port}"
        exit
        ;;
    __request)
        run_request
        exit
        ;;
    __summary)
        run_summary
        exit
        ;;
esac

SESSION="rnc-substitute-demo"
MANAGER_PORT=52337
PACKAGE="nixpkgs#hello"
DETACH=false

usage() {
    cat <<'EOF'
Usage: examples/demo_substitute_tmux.sh [OPTIONS]

Run seven real Builders plus three liars with substitute=true.

Options:
  --detach            Run detached, wait for PASS/FAIL, and return its status
  --manager-port PORT Manager listens on 0.0.0.0:PORT (default: 52337)
  --package REF       Nix package reference (default: nixpkgs#hello)
  --session NAME      tmux session name (default: rnc-substitute-demo)
  -h, --help          Show this help
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --detach)
            DETACH=true
            shift
            ;;
        --manager-port)
            MANAGER_PORT=${2:?--manager-port requires a value}
            shift 2
            ;;
        --package)
            PACKAGE=${2:?--package requires a value}
            shift 2
            ;;
        --session)
            SESSION=${2:?--session requires a value}
            shift 2
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            printf 'unknown option: %s\n' "$1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if [[ ! "$MANAGER_PORT" =~ ^[0-9]+$ ]] \
    || ((MANAGER_PORT < 1 || MANAGER_PORT + TOTAL_PARTICIPANT_COUNT > 65535)); then
    printf 'invalid Manager port: %s\n' "$MANAGER_PORT" >&2
    exit 2
fi
if [[ "$PACKAGE" != *#* ]]; then
    printf 'package must use repository#attribute syntax: %s\n' "$PACKAGE" >&2
    exit 2
fi
if [[ ! "$SESSION" =~ ^[A-Za-z0-9_.-]+$ ]]; then
    printf 'invalid tmux session name: %s\n' "$SESSION" >&2
    exit 2
fi

for command in cargo curl nix python3 tmux; do
    if ! command -v "$command" >/dev/null; then
        printf 'missing command: %s\n' "$command" >&2
        printf 'Run this demo with: nix develop -c ./examples/demo_substitute_tmux.sh\n' >&2
        exit 1
    fi
done

if tmux has-session -t "$SESSION" 2>/dev/null; then
    printf 'tmux session already exists: %s\n' "$SESSION" >&2
    printf 'Attach with: tmux attach-session -t %s\n' "$SESSION" >&2
    exit 1
fi

if ! python3 - "$MANAGER_PORT" "$TOTAL_PARTICIPANT_COUNT" <<'PY'
import socket
import sys

manager_port = int(sys.argv[1])
builder_count = int(sys.argv[2])
listeners = []
try:
    for port in range(manager_port, manager_port + builder_count + 1):
        listener = socket.socket()
        listener.bind(("127.0.0.1", port))
        listeners.append(listener)
finally:
    for listener in listeners:
        listener.close()
PY
then
    printf 'one of ports %s-%s is already in use\n' \
        "$MANAGER_PORT" "$((MANAGER_PORT + TOTAL_PARTICIPANT_COUNT))" >&2
    exit 1
fi

printf 'Building Round Manager and Builder Node binaries...\n'
(cd "$REPOSITORY_ROOT" && cargo build -p server -p builder-node)

printf 'Resolving the derivation path for liar commitments...\n'
DERIVATION_PATH="$(
    nix path-info --derivation --json-format 1 --json "$PACKAGE" \
        | python3 -c 'import json, sys; data = json.load(sys.stdin); assert len(data) == 1; print(next(iter(data)))'
)"

DEMO_DIR="$(mktemp -d "/tmp/reproductive-substitute-demo.XXXXXX")"
DEMO_MANAGER_URL="http://127.0.0.1:${MANAGER_PORT}"
DEMO_MANAGER_HOST="127.0.0.1:${MANAGER_PORT}"
DEMO_RESULT="$DEMO_DIR/result"

tmux new-session -d -x 200 -y 60 -s "$SESSION" -n control 'sleep 86400'
created_session=true
trap 'if [[ ${created_session:-false} == true ]]; then tmux kill-session -t "$SESSION" 2>/dev/null || true; fi' ERR

tmux set-environment -t "$SESSION" DEMO_ROOT "$REPOSITORY_ROOT"
tmux set-environment -t "$SESSION" DEMO_DIR "$DEMO_DIR"
tmux set-environment -t "$SESSION" DEMO_MANAGER_URL "$DEMO_MANAGER_URL"
tmux set-environment -t "$SESSION" DEMO_MANAGER_HOST "$DEMO_MANAGER_HOST"
tmux set-environment -t "$SESSION" DEMO_MANAGER_PORT "$MANAGER_PORT"
tmux set-environment -t "$SESSION" DEMO_HONEST_BUILDER_COUNT "$HONEST_BUILDER_COUNT"
tmux set-environment -t "$SESSION" DEMO_LIAR_COUNT "$LIAR_COUNT"
tmux set-environment -t "$SESSION" DEMO_TOTAL_PARTICIPANT_COUNT "$TOTAL_PARTICIPANT_COUNT"
tmux set-environment -t "$SESSION" DEMO_PACKAGE "$PACKAGE"
tmux set-environment -t "$SESSION" DEMO_DERIVATION_PATH "$DERIVATION_PATH"
tmux set-environment -t "$SESSION" DEMO_RESULT "$DEMO_RESULT"
tmux set-environment -t "$SESSION" DEMO_SESSION "$SESSION"
tmux set-option -t "$SESSION" remain-on-exit on
tmux set-option -t "$SESSION" pane-border-status top
tmux set-option -t "$SESSION" pane-border-format ' #{pane_title} '

printf -v manager_command '%q __manager' "$SCRIPT_PATH"
printf -v request_command '%q __request' "$SCRIPT_PATH"
printf -v summary_command '%q __summary' "$SCRIPT_PATH"

manager_pane="$(tmux display-message -p -t "$SESSION:control.0" '#{pane_id}')"
tmux respawn-pane -k -t "$manager_pane" "$manager_command"
request_pane="$(tmux split-window -h -P -F '#{pane_id}' -t "$manager_pane" "$request_command")"
summary_pane="$(tmux split-window -v -P -F '#{pane_id}' -t "$request_pane" "$summary_command")"
tmux select-layout -t "$SESSION:control" main-vertical >/dev/null
tmux select-pane -t "$manager_pane" -T 'Round Manager'
tmux select-pane -t "$request_pane" -T 'curl + E2E assertion'
tmux select-pane -t "$summary_pane" -T 'demo result'

tmux new-window -d -t "$SESSION" -n builders 'sleep 86400'
first_builder_pane="$(tmux display-message -p -t "$SESSION:builders.0" '#{pane_id}')"
for index in $(seq 1 "$HONEST_BUILDER_COUNT"); do
    port=$((MANAGER_PORT + index))
    printf -v builder_command '%q __builder %q %q' "$SCRIPT_PATH" "$index" "$port"
    if [[ "$index" == 1 ]]; then
        pane=$first_builder_pane
        tmux respawn-pane -k -t "$pane" "$builder_command"
    else
        pane="$(tmux split-window -P -F '#{pane_id}' -t "$SESSION:builders" "$builder_command")"
    fi
    printf -v builder_title 'Builder %02d · %d' "$index" "$port"
    tmux select-pane -t "$pane" -T "$builder_title"
    tmux select-layout -t "$SESSION:builders" tiled >/dev/null
done

tmux new-window -d -t "$SESSION" -n liars 'sleep 86400'
first_liar_pane="$(tmux display-message -p -t "$SESSION:liars.0" '#{pane_id}')"
for index in $(seq 1 "$LIAR_COUNT"); do
    port=$((MANAGER_PORT + HONEST_BUILDER_COUNT + index))
    printf -v liar_command '%q __liar %q %q' "$SCRIPT_PATH" "$index" "$port"
    if [[ "$index" == 1 ]]; then
        pane=$first_liar_pane
        tmux respawn-pane -k -t "$pane" "$liar_command"
    else
        pane="$(tmux split-window -P -F '#{pane_id}' -t "$SESSION:liars" "$liar_command")"
    fi
    printf -v liar_title 'Liar %02d · %d' "$index" "$port"
    tmux select-pane -t "$pane" -T "$liar_title"
    tmux select-layout -t "$SESSION:liars" tiled >/dev/null
done

tmux select-window -t "$SESSION:control"
tmux select-pane -t "$request_pane"
created_session=false
trap - ERR

printf 'tmux session: %s\n' "$SESSION"
printf 'Graph UI:    %s/\n' "$DEMO_MANAGER_URL"
printf 'Honest:      %s real Builders on ports %s-%s\n' \
    "$HONEST_BUILDER_COUNT" "$((MANAGER_PORT + 1))" \
    "$((MANAGER_PORT + HONEST_BUILDER_COUNT))"
printf 'Liars:       %s malicious Builder Nodes on ports %s-%s\n' \
    "$LIAR_COUNT" "$((MANAGER_PORT + HONEST_BUILDER_COUNT + 1))" \
    "$((MANAGER_PORT + TOTAL_PARTICIPANT_COUNT))"
printf 'Logs:        %s\n' "$DEMO_DIR"

if [[ "$DETACH" == false ]]; then
    if [[ -n "${TMUX:-}" ]]; then
        exec tmux switch-client -t "$SESSION"
    fi
    exec tmux attach-session -t "$SESSION"
fi

printf 'Waiting for the detached E2E result...\n'
for _ in $(seq 1 12000); do
    if [[ -f "$DEMO_RESULT" ]]; then
        result="$(cat "$DEMO_RESULT")"
        printf '%s\n' "$result"
        [[ "$result" == PASS:* ]]
        exit
    fi
    if ! tmux has-session -t "$SESSION" 2>/dev/null; then
        printf 'tmux session ended before producing a result\n' >&2
        exit 1
    fi
    sleep 0.1
done

printf 'timed out waiting for the E2E result\n' >&2
exit 1
