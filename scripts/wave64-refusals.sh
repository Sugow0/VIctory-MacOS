#!/bin/bash
# The console wave64 compute programs the translator refuses on D3DMetal
# (waves of 32 only: "a wave size wider than the host's waves"), offline and
# native: one line each with hash, stage, thread group, class and the
# WAVE64-WHY / WAVE64-RETRY diagnostics of every attempt, then a count per
# class. Re-run it after a translator change to re-count.
#
# Usage: scripts/wave64-refusals.sh [out.txt] [paths...]
#   out.txt  default: ~/victory-data/wave64-refusals.txt
#   paths    default: /Volumes/Save1/GTAVI/Victory/GTA6/shaders.rpf (read only)
# Env: VICTORY_BIN (default: target/aarch64-apple-darwin/release/victory, built
# if missing); other VICTORY_* variables (VICTORY_WAVE_WIDE=1, ...) pass through.
#
# Classes, from the diagnostics of the program as it is (its first WAVE64-WHY):
#   (a) an indirect exit (`all_lane_blocks` gives none)
#   (b) LDS read and written inside a per-lane branch in a group over 32
#       (`lds_meetings` gives none)
#   other: anything else
# and "+c" when one of the retried forms got past that but stopped at
# cross-lane code where not every lane runs (`widen` off or not possible).
set -euo pipefail

# (paths as the caller gave them, before the cd to the repository)
abs() { case $1 in /*) printf '%s\n' "$1" ;; *) printf '%s\n' "$PWD/$1" ;; esac; }
out=$(abs "${1:-$HOME/victory-data/wave64-refusals.txt}")
shift || true
[ $# -gt 0 ] || set -- /Volumes/Save1/GTAVI/Victory/GTA6/shaders.rpf
paths=()
for p in "$@"; do paths+=("$(abs "$p")"); done
set -- "${paths[@]}"
cd "$(dirname "$0")/.."
mkdir -p "$(dirname "$out")"
bin=${VICTORY_BIN:-target/aarch64-apple-darwin/release/victory}
if [ -z "${VICTORY_BIN:-}" ]; then
    PATH="$HOME/.cargo/bin:$PATH" cargo build --release -p victory --target aarch64-apple-darwin >&2
fi

raw=$(mktemp)
trap 'rm -f "$raw"' EXIT
VICTORY_HOST_WAVE=32 VICTORY_HOST_D3DMETAL=1 "$bin" translate --list "$@" >"$raw"

{
    echo "# wave64 refusals, host waves of 32 (D3DMetal), $(date '+%Y-%m-%d %H:%M')"
    echo "# translator: $(git rev-parse --short HEAD)$(git diff --quiet HEAD -- crates apps || echo '+dirty'), paths: $*"
    echo "# hash  stage  threads  class  name  |  diagnostics of every attempt"
    { LC_ALL=C grep '  wave64 threads=' "$raw" || true; } | awk '
    BEGIN { total = 0 }
    {
        hash = ""; stage = ""
        for (i = 1; i <= NF; i++) {
            if ($i ~ /^#[0-9a-f]+$/) { hash = substr($i, 2); stage = $(i + 2) }
        }
        # (the whole label: after the archive offset, up to the hash)
        name = $0
        sub(/ #[0-9a-f]+ ~.*/, "", name)
        sub(/^.*@0x[0-9a-f]+ /, "", name)
        line = $0
        sub(/.*  wave64 threads=/, "", line)
        threads = line; sub(/: .*/, "", threads); gsub(/ /, "", threads)
        diag = line; sub(/^[^:]*: /, "", diag)
        first = diag; sub(/ \| .*/, "", first)
        if (first ~ /an indirect exit/) class = "a"
        else if (first ~ /an LDS read and write in one lane-branch region/) class = "b"
        else class = "other"
        if (diag ~ /where not every lane runs/) class = class "+c"
        printf "%s  %s  %s  %s  %s  |  %s\n", hash, stage, threads, class, name, diag
        n[class]++; total++
    }
    END {
        printf "# refused: %d\n", total
        for (c in n) printf "# class %s: %d\n", c, n[c]
    }'
    echo "# summary of the whole run:"
    LC_ALL=C grep -E "^ +[0-9]+  emit: a wave size wider" "$raw" | sed 's/  (e\.g\..*//; s/^/# /' || echo "#     0  emit: a wave size wider than the host's waves"
} >"$out"
LC_ALL=C grep '^# ' "$out" | tail -n +4
echo "written: $out"
