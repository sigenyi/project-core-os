#!/usr/bin/env bash
# Build the ISO from any Linux host with podman or docker.
# mkarchiso needs loop devices and mounts, hence --privileged.
#
#   image/build-in-container.sh [build-iso.sh options...]
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/.." && pwd)"
engine="${CONTAINER_ENGINE:-$(command -v podman || command -v docker || true)}"
[[ -n "$engine" ]] || { echo "podman or docker is required" >&2; exit 1; }

"$engine" build -t core-os-builder -f "$here/Containerfile" "$here"
"$engine" run --rm --privileged \
    -v "$repo:/src" \
    -v core-os-cache:/root/.cache \
    -e CACHE_DIR=/root/.cache/core-os-image \
    core-os-builder /src/image/build-iso.sh "$@"
