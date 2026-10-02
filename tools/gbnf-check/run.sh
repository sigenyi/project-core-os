#!/usr/bin/env bash
# Build gbnf-check against a llama.cpp checkout and verify C.O.R.E.'s intent grammar.
#
#   LLAMA_CPP_DIR=/path/to/llama.cpp tools/gbnf-check/run.sh
#
# The llama.cpp tree must already be configured and built with static libraries:
#   cmake -B build -DBUILD_SHARED_LIBS=OFF -DLLAMA_CURL=OFF && cmake --build build --target llama
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
llama="${LLAMA_CPP_DIR:?set LLAMA_CPP_DIR to a built llama.cpp checkout}"
build="${LLAMA_BUILD_DIR:-$llama/build}"
work="${TMPDIR:-/tmp}/core-gbnf-check"
mkdir -p "$work"

libs=("$build/src/libllama.a" "$build/ggml/src/libggml.a" "$build/ggml/src/libggml-cpu.a" "$build/ggml/src/libggml-base.a")
for lib in "${libs[@]}"; do
    [[ -f "$lib" ]] || { echo "missing $lib; build llama.cpp with -DBUILD_SHARED_LIBS=OFF" >&2; exit 2; }
done

"${CXX:-c++}" -std=c++17 -O1 \
    -I "$llama/include" -I "$llama/src" -I "$llama/ggml/include" \
    "$here/gbnf-check.cpp" "${libs[@]}" -lpthread -fopenmp -o "$work/gbnf-check"

cargo run -q --manifest-path "$repo/Cargo.toml" -p core-protocol --example gbnf > "$work/intent.gbnf"
cargo run -q --manifest-path "$repo/Cargo.toml" -p core-protocol --example gbnf -- --cases > "$work/cases.txt"
"$work/gbnf-check" "$work/intent.gbnf" "$work/cases.txt"
