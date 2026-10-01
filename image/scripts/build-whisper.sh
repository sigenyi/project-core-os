#!/usr/bin/env bash
# Build whisper.cpp (whisper-cli and whisper-server) for C.O.R.E. OS voice input.
#
#   image/scripts/build-whisper.sh --prefix /usr/lib/core/whisper
#
# Same portable CPU strategy as build-llama.sh.
set -euo pipefail

repo_url="${WHISPER_REPO:-https://github.com/ggml-org/whisper.cpp}"
ref="${WHISPER_REF:-v1.9.4}"
src="${WHISPER_SRC:-${XDG_CACHE_HOME:-$HOME/.cache}/core-os/whisper.cpp}"
prefix=""
jobs="$(nproc)"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --prefix) prefix="$2"; shift 2 ;;
        --ref) ref="$2"; shift 2 ;;
        --src) src="$2"; shift 2 ;;
        --jobs) jobs="$2"; shift 2 ;;
        -h|--help) echo "usage: $0 --prefix DIR [--ref TAG] [--src DIR] [--jobs N]"; exit 0 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done
[[ -n "$prefix" ]] || { echo "--prefix is required" >&2; exit 2; }

if [[ ! -d "$src/.git" ]]; then
    mkdir -p "$(dirname "$src")"
    git clone --depth 1 --branch "$ref" "$repo_url" "$src"
else
    git -C "$src" fetch --depth 1 origin "refs/tags/$ref:refs/tags/$ref" 2>/dev/null || true
    git -C "$src" checkout --quiet "$ref"
fi

build="$src/build-core"
cmake_args=(
    -S "$src" -B "$build"
    -DCMAKE_BUILD_TYPE=Release
    -DBUILD_SHARED_LIBS=ON
    -DCMAKE_BUILD_RPATH_USE_ORIGIN=ON
    -DGGML_NATIVE=OFF
    -DGGML_BACKEND_DL=ON
    -DGGML_CPU_ALL_VARIANTS=ON
    -DWHISPER_BUILD_TESTS=OFF
    -DWHISPER_BUILD_EXAMPLES=ON
    -DWHISPER_BUILD_SERVER=ON
    -DWHISPER_SDL2=OFF
    -DWHISPER_CURL=OFF
)
command -v ninja >/dev/null && cmake_args+=(-G Ninja)

cmake "${cmake_args[@]}"
# Build everything: CPU backend modules are not dependencies of the executables.
cmake --build "$build" --config Release -j "$jobs"

mkdir -p "$prefix/bin"
install -m755 "$build/bin/whisper-cli" "$build/bin/whisper-server" "$prefix/bin/"
find "$build/bin" "$build/src" "$build/ggml/src" -maxdepth 1 -name '*.so*' -exec cp -a {} "$prefix/bin/" \; 2>/dev/null || true
echo "installed whisper.cpp $ref to $prefix"
