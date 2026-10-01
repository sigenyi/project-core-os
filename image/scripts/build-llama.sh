#!/usr/bin/env bash
# Build llama.cpp's llama-server for C.O.R.E. OS and install it under a prefix.
#
# The build is portable: CPU code for every x86-64 level is compiled as separate
# backends and the best one for the running machine is picked at runtime
# (GGML_BACKEND_DL + GGML_CPU_ALL_VARIANTS). --vulkan adds GPU offload for AMD,
# Intel and NVIDIA GPUs through Vulkan.
#
#   image/scripts/build-llama.sh --prefix /usr/lib/core/llama [--vulkan]
#
# Result: PREFIX/bin/llama-server plus the shared libraries it loads ($ORIGIN rpath).
set -euo pipefail

repo_url="${LLAMA_REPO:-https://github.com/ggml-org/llama.cpp}"
ref="${LLAMA_REF:-b11312}"
src="${LLAMA_SRC:-${XDG_CACHE_HOME:-$HOME/.cache}/core-os/llama.cpp}"
prefix=""
vulkan=0
jobs="$(nproc)"

usage() {
    sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'
    echo "options: --prefix DIR (required) --ref TAG --src DIR --vulkan --jobs N"
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --prefix) prefix="$2"; shift 2 ;;
        --ref) ref="$2"; shift 2 ;;
        --src) src="$2"; shift 2 ;;
        --vulkan) vulkan=1; shift ;;
        --jobs) jobs="$2"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done
[[ -n "$prefix" ]] || { usage >&2; exit 2; }

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
    -DLLAMA_CURL=OFF
    -DLLAMA_BUILD_TESTS=OFF
    -DLLAMA_BUILD_EXAMPLES=OFF
    -DLLAMA_BUILD_TOOLS=ON
    -DLLAMA_BUILD_SERVER=ON
)
if [[ "$vulkan" == 1 ]]; then
    cmake_args+=(-DGGML_VULKAN=ON)
fi
command -v ninja >/dev/null && cmake_args+=(-G Ninja)

cmake "${cmake_args[@]}"
cmake --build "$build" --config Release -j "$jobs"

# Everything (executables, libllama, libggml*, CPU/GPU backend modules) lands in
# build/bin with $ORIGIN rpaths; ship the server, the CLI (for debugging) and libs.
mkdir -p "$prefix/bin"
install -m755 "$build/bin/llama-server" "$prefix/bin/"
[[ -x "$build/bin/llama-cli" ]] && install -m755 "$build/bin/llama-cli" "$prefix/bin/"
find "$build/bin" -maxdepth 1 -name '*.so*' -exec cp -a {} "$prefix/bin/" \;
echo "installed llama.cpp $ref to $prefix"
"$prefix/bin/llama-server" --version 2>&1 | tail -n 2 || true
