#!/usr/bin/env bash
# Download (and verify) the models listed in image/models.conf.
#
#   image/scripts/fetch-models.sh --dest DIR [--manifest FILE] [--only ROLE] [--allow-unpinned]
#
# Creates DIR/<file> for each model plus the stable names the services use:
# DIR/core.gguf (role llm) and DIR/whisper.bin (role whisper).
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
manifest="$here/../models.conf"
dest=""
only=""
allow_unpinned=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dest) dest="$2"; shift 2 ;;
        --manifest) manifest="$2"; shift 2 ;;
        --only) only="$2"; shift 2 ;;
        --allow-unpinned) allow_unpinned=1; shift ;;
        -h|--help) sed -n '2,8p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done
[[ -n "$dest" ]] || { echo "--dest is required" >&2; exit 2; }
mkdir -p "$dest"
manifest_name="$(basename "$manifest")"

link_name() {
    case "$1" in
        llm) echo core.gguf ;;
        whisper) echo whisper.bin ;;
        *) echo "" ;;
    esac
}

while read -r role file sha url; do
    [[ -z "${role:-}" || "$role" == \#* ]] && continue
    [[ -n "$only" && "$role" != "$only" ]] && continue
    target="$dest/$file"
    if [[ -f "$target" && "$sha" != "-" ]] && echo "$sha  $target" | sha256sum --check --status; then
        echo "ok      $file (verified)"
    elif [[ -f "$target" && "$sha" == "-" ]]; then
        echo "ok      $file (present, unpinned)"
    else
        echo "fetch   $file"
        curl --fail --location --retry 3 --progress-bar --continue-at - --output "$target.part" "$url"
        actual="$(sha256sum "$target.part" | cut -d' ' -f1)"
        if [[ "$sha" == "-" ]]; then
            echo "        sha256 $actual  (pin this in $manifest_name)"
            if [[ "$allow_unpinned" != 1 ]]; then
                echo "refusing unpinned model; re-run with --allow-unpinned to accept it" >&2
                exit 1
            fi
        elif [[ "$actual" != "$sha" ]]; then
            rm -f "$target.part"
            echo "checksum mismatch for $file: expected $sha, got $actual" >&2
            exit 1
        fi
        mv "$target.part" "$target"
    fi
    link="$(link_name "$role")"
    [[ -n "$link" ]] && ln -sfn "$file" "$dest/$link"
done < "$manifest"
echo "models ready in $dest"
