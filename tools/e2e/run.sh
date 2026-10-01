#!/usr/bin/env bash
# End-to-end test of the model path with a real llama.cpp server.
#
#   LLAMA_CPP_DIR=/path/to/llama.cpp tools/e2e/run.sh
#
# Needs a llama.cpp checkout (for gguf-py and the vocab test files) with llama-server
# built (LLAMA_SERVER overrides its location), plus python3 with numpy.
# Steps: synthesise a tiny random-weight model, serve it, check that llama-server
# honours C.O.R.E.'s grammar, then drive one full request through core-shell (dev
# mode: in-process dry-run Guardian, so nothing on this machine is changed).
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
llama="${LLAMA_CPP_DIR:?set LLAMA_CPP_DIR to a llama.cpp checkout}"
server_bin="${LLAMA_SERVER:-$llama/build/bin/llama-server}"
port="${E2E_PORT:-18080}"
work="$(mktemp -d "${TMPDIR:-/tmp}/core-e2e.XXXXXX")"
server_pid=""

cleanup() {
    [[ -n "$server_pid" ]] && kill "$server_pid" 2>/dev/null || true
    rm -rf "$work"
}
trap cleanup EXIT

[[ -x "$server_bin" ]] || { echo "llama-server not found at $server_bin" >&2; exit 2; }

echo "==> synthesising a tiny model"
PYTHONPATH="$llama/gguf-py" python3 "$here/make-tiny-model.py" \
    --vocab "$llama/models/ggml-vocab-llama-spm.gguf" --out "$work/tiny.gguf"

echo "==> starting llama-server on port $port"
"$server_bin" --model "$work/tiny.gguf" --host 127.0.0.1 --port "$port" \
    --ctx-size 8192 --parallel 1 --jinja --no-webui > "$work/server.log" 2>&1 &
server_pid=$!
url="http://127.0.0.1:$port"
for _ in $(seq 1 60); do
    curl -sf --noproxy '*' "$url/health" >/dev/null && break
    sleep 1
done
curl -sf --noproxy '*' "$url/health" >/dev/null || { cat "$work/server.log" >&2; exit 1; }

echo "==> grammar conformance against the live server"
CORE_E2E_LLAMA_URL="$url" cargo test --manifest-path "$repo/Cargo.toml" -q \
    -p core-agent --test llama_e2e -- --ignored

echo "==> one full request through core-shell (dev mode)"
cat > "$work/agent.toml" <<EOF
[inference]
backend = "llama"
url = "$url"
max_tokens = 96
fallback_to_rescue = false
[agent]
max_steps = 3
EOF
cargo build --manifest-path "$repo/Cargo.toml" -q -p core-shell
set +e
output="$(echo "" | timeout 600 "$repo/target/debug/core-shell" --dev --config "$work/agent.toml" -c "check disk usage" 2>&1)"
status=$?
set -e
echo "$output" | tail -n 12
if [[ $status -gt 1 ]]; then
    echo "core-shell exited with status $status" >&2
    exit 1
fi
echo "==> end-to-end test passed"
