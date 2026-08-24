#!/usr/bin/env bash
set -euo pipefail

SCRIPT_PATH="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"
REPOSITORY_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

write_result() {
    local message=$1
    printf '%s\n' "$message" >"${DEMO_RESULT}.tmp"
    mv "${DEMO_RESULT}.tmp" "$DEMO_RESULT"
}

run_server() {
    cd "$DEMO_ROOT"
    RUST_LOG=reproductive_nix_cache_server=debug \
        "$DEMO_ROOT/target/debug/reproductive-nix-cache-server" \
        --listen "0.0.0.0:${DEMO_PORT}" \
        --database "$DEMO_DIR/demo.sqlite" \
        --commit-min-builders 15 \
        --cache-min-builders 10 \
        --commit-window-seconds 600 \
        --reveal-window-seconds 600 \
        2>&1 | tee "$DEMO_DIR/server.log"
}

run_scenario() {
    printf 'Waiting for %s/healthz' "$DEMO_URL"
    for _ in $(seq 1 150); do
        if curl -fsS "$DEMO_URL/healthz" >/dev/null 2>&1; then
            printf ' ready\n\n'
            if python3 "$DEMO_ROOT/examples/demo_15_builders.py" \
                --server "$DEMO_URL" 2>&1 | tee "$DEMO_DIR/scenario.log"; then
                write_result "PASS: 15 reveals, 10 correct, 5 fake, 2 cache nodes"
                exit 0
            else
                status=${PIPESTATUS[0]}
                write_result "FAIL: scenario exited with status ${status}"
                exit "$status"
            fi
        fi
        printf '.'
        sleep 0.1
    done

    printf '\nServer did not become healthy.\n'
    write_result "FAIL: server did not become healthy"
    exit 1
}

run_overview() {
    while [[ ! -f "$DEMO_RESULT" ]]; do
        printf '\033[H\033[2J'
        printf 'OVERVIEW API · %s/v1/overview\n\n' "$DEMO_URL"
        python3 - "$DEMO_URL" <<'PY' || true
import collections
import json
import sys
import urllib.request

try:
    with urllib.request.urlopen(sys.argv[1] + "/v1/overview", timeout=1) as response:
        overview = json.load(response)
except Exception as error:
    print(f"waiting: {error}")
    raise SystemExit(0)

if not overview["rounds"]:
    print("waiting for the first round")
    raise SystemExit(0)

round_ = overview["rounds"][0]
groups = collections.Counter()
caches = collections.defaultdict(set)
for participant in round_["participants"]:
    outputs = participant["outputs"]
    if not outputs:
        continue
    nar_hash = outputs[0]["nar_hash"]
    groups[nar_hash] += 1
    caches[nar_hash].update(participant["cache_locations"])

print(f"round:   #{round_['id']} {round_['phase']}")
print(f"commits: {round_['commit_count']} / 15")
print(f"reveals: {round_['reveal_count']} / 15")
for nar_hash, count in groups.most_common():
    print(f"\n{count:2} builders  {nar_hash}")
    for uri in sorted(caches[nar_hash]):
        print(f"             -> {uri}")
PY
        sleep 0.5
    done

    printf '\033[H\033[2J'
    printf 'OVERVIEW API · final\n\n'
    cat "$DEMO_RESULT"
    printf '\nWeb UI: %s/\n' "$DEMO_URL"
}

run_summary() {
    printf 'FAST UI E2E DEMO\n\n'
    printf 'Web UI:     %s/\n' "$DEMO_URL"
    printf 'Session:    %s\n' "$DEMO_SESSION"
    printf 'Logs:       %s\n\n' "$DEMO_DIR"
    printf 'The graph refreshes every four seconds.\n'
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
    __server)
        run_server
        exit
        ;;
    __scenario)
        run_scenario
        exit
        ;;
    __overview)
        run_overview
        exit
        ;;
    __summary)
        run_summary
        exit
        ;;
esac

SESSION="rnc-ui-demo"
PORT=5123
DETACH=false

usage() {
    cat <<'EOF'
Usage: examples/demo_ui_tmux.sh [OPTIONS]

Run the fast 15-builder Graph View E2E demo in a tmux session.

Options:
  --detach          Run detached, wait for PASS/FAIL, and return its status
  --port PORT       Listen on 0.0.0.0:PORT (default: 5123)
  --session NAME    tmux session name (default: rnc-ui-demo)
  -h, --help        Show this help
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --detach)
            DETACH=true
            shift
            ;;
        --port)
            PORT=${2:?--port requires a value}
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

if [[ ! "$PORT" =~ ^[0-9]+$ ]] || ((PORT < 1 || PORT > 65535)); then
    printf 'invalid port: %s\n' "$PORT" >&2
    exit 2
fi
if [[ ! "$SESSION" =~ ^[A-Za-z0-9_.-]+$ ]]; then
    printf 'invalid tmux session name: %s\n' "$SESSION" >&2
    exit 2
fi

for command in cargo curl python3 tmux; do
    if ! command -v "$command" >/dev/null; then
        printf 'missing command: %s\n' "$command" >&2
        printf 'Run this demo with: nix develop -c ./examples/demo_ui_tmux.sh\n' >&2
        exit 1
    fi
done

if tmux has-session -t "$SESSION" 2>/dev/null; then
    printf 'tmux session already exists: %s\n' "$SESSION" >&2
    printf 'Attach with: tmux attach-session -t %s\n' "$SESSION" >&2
    exit 1
fi

if ! python3 - "$PORT" <<'PY'
import socket
import sys

with socket.socket() as listener:
    listener.bind(("127.0.0.1", int(sys.argv[1])))
PY
then
    printf 'port is already in use: %s\n' "$PORT" >&2
    exit 1
fi

printf 'Building the server...\n'
(cd "$REPOSITORY_ROOT" && cargo build -p server)

DEMO_DIR="$(mktemp -d "/tmp/reproductive-ui-demo.XXXXXX")"
DEMO_URL="http://127.0.0.1:${PORT}"
DEMO_RESULT="$DEMO_DIR/result"

tmux new-session -d -x 180 -y 48 -s "$SESSION" -n demo 'sleep 86400'
created_session=true
trap 'if [[ ${created_session:-false} == true ]]; then tmux kill-session -t "$SESSION" 2>/dev/null || true; fi' ERR

tmux set-environment -t "$SESSION" DEMO_ROOT "$REPOSITORY_ROOT"
tmux set-environment -t "$SESSION" DEMO_DIR "$DEMO_DIR"
tmux set-environment -t "$SESSION" DEMO_URL "$DEMO_URL"
tmux set-environment -t "$SESSION" DEMO_PORT "$PORT"
tmux set-environment -t "$SESSION" DEMO_RESULT "$DEMO_RESULT"
tmux set-environment -t "$SESSION" DEMO_SESSION "$SESSION"
tmux set-option -t "$SESSION" remain-on-exit on
tmux set-option -t "$SESSION" pane-border-status top
tmux set-option -t "$SESSION" pane-border-format ' #{pane_title} '

printf -v server_command '%q __server' "$SCRIPT_PATH"
printf -v scenario_command '%q __scenario' "$SCRIPT_PATH"
printf -v overview_command '%q __overview' "$SCRIPT_PATH"
printf -v summary_command '%q __summary' "$SCRIPT_PATH"

server_pane="$(tmux display-message -p -t "$SESSION:demo.0" '#{pane_id}')"
tmux respawn-pane -k -t "$server_pane" "$server_command"
scenario_pane="$(tmux split-window -h -P -F '#{pane_id}' -t "$server_pane" "$scenario_command")"
overview_pane="$(tmux split-window -v -P -F '#{pane_id}' -t "$server_pane" "$overview_command")"
summary_pane="$(tmux split-window -v -P -F '#{pane_id}' -t "$scenario_pane" "$summary_command")"
tmux select-layout -t "$SESSION:demo" tiled >/dev/null
tmux select-pane -t "$server_pane" -T 'server log'
tmux select-pane -t "$scenario_pane" -T '15-builder commit/reveal'
tmux select-pane -t "$overview_pane" -T 'overview API'
tmux select-pane -t "$summary_pane" -T 'demo result'
tmux select-pane -t "$scenario_pane"

created_session=false
trap - ERR

printf 'tmux session: %s\n' "$SESSION"
printf 'Web UI:      %s/\n' "$DEMO_URL"
printf 'Logs:        %s\n' "$DEMO_DIR"

if [[ "$DETACH" == false ]]; then
    if [[ -n "${TMUX:-}" ]]; then
        exec tmux switch-client -t "$SESSION"
    fi
    exec tmux attach-session -t "$SESSION"
fi

printf 'Waiting for the detached E2E result...\n'
for _ in $(seq 1 600); do
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
