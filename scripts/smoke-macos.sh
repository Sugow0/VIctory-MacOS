#!/bin/bash
# End-to-end smoke test on macOS: cross-builds victory.exe and the synthetic
# title (tests/fixtures/smoke-title), runs `victory run <title> --execute`
# under Wine (CrossOver's, D3D12 by D3DMetal), and checks that the title
# exited with 0 and that the frame meter logged frame times; prints their
# median. No game data. (apps/victory/tests/smoke_title.rs is the same check
# on Windows.)
#   scripts/smoke-macos.sh [--frames N] [--unpaced] [--clears N] [--copies N] [--draws N] [--env VICTORY_X=V]... [--exit-code C] [--build release|diag] [--no-build] [--timeout S]
# --frames N      frames the title presents (default 1200: 20 s at its 60 Hz; 3600 unpaced; the meter logs every 5 s)
# --unpaced       no frame clock (VICTORY_FRAME_INTERVAL_MS=0): as fast as presents go (vsync: 120 Hz on a ProMotion Mac)
# --clears N      a load: N more clears of the target per frame (default 0)
# --copies N      a load: N copies of the target per frame, each between two barriers (default 0)
# --draws N       a load: N draws of a small triangle per frame, console shaders the fixture's build makes (default 0);
#                 the check then also wants draws on the host (the meter's draws/frame above 0 on every line)
#                 (the loads need a title built from this script's fixture: --no-build with an older one runs without)
# --env K=V       a VIctory switch for the run (K starts with VICTORY_; repeatable; e.g. VICTORY_GPU_TIMING=1 with --build diag)
# --exit-code C   the title returns C (non-zero: the check must fail)
# --build B       release (the default) or diag (diagnostics, as play-macos.sh plays)
# --no-build      use what the last build left
# --timeout S     seconds before the run is killed (default 180)
# Environment, as scripts/play-macos.sh takes it: WINE (default CrossOver's,
# in /Applications or ~/Applications), CX_BOTTLE (CrossOver's bottle with
# D3DMetal on; default victory), WINEPREFIX (another Wine's; default
# ~/.victory-wine), CARGO_TARGET_DIR (default target). Exit status: 0 passed, 1 failed.
# Written for macOS's own /bin/bash (3.2).
set -euo pipefail
# Decimal points in the numbers read and printed, whatever the locale.
export LC_ALL=C

usage() { sed -n '2,25p' "$0" | sed 's/^# \{0,1\}//'; }
die() { echo "smoke-macos: $*" >&2; exit 1; }

frames=
unpaced=0
exit_code=0
clears=0
copies=0
draws=0
switches=()
build=release
do_build=1
timeout=180
while [ $# -gt 0 ]; do
    case "$1" in
        --frames) [ $# -ge 2 ] || die "--frames needs a number"; frames=$2; shift 2 ;;
        --unpaced) unpaced=1; shift ;;
        --clears) [ $# -ge 2 ] || die "--clears needs a number"; clears=$2; shift 2 ;;
        --copies) [ $# -ge 2 ] || die "--copies needs a number"; copies=$2; shift 2 ;;
        --draws) [ $# -ge 2 ] || die "--draws needs a number"; draws=$2; shift 2 ;;
        --env)
            [ $# -ge 2 ] || die "--env needs VICTORY_X=V"
            case "$2" in VICTORY_*=*) ;; *) die "--env takes VICTORY_X=V, not '$2'" ;; esac
            switches+=("$2"); shift 2 ;;
        --exit-code) [ $# -ge 2 ] || die "--exit-code needs a number"; exit_code=$2; shift 2 ;;
        --build) [ $# -ge 2 ] || die "--build needs release or diag"; build=$2; shift 2 ;;
        --no-build) do_build=0; shift ;;
        --timeout) [ $# -ge 2 ] || die "--timeout needs seconds"; timeout=$2; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown argument '$1'" ;;
    esac
done
[ -n "$frames" ] || { [ "$unpaced" = 1 ] && frames=3600 || frames=1200; }
case "$frames" in ''|*[!0-9]*) die "--frames takes a whole number" ;; esac
case "$exit_code" in ''|*[!0-9]*) die "--exit-code takes a whole number" ;; esac
case "$clears" in ''|*[!0-9]*) die "--clears takes a whole number" ;; esac
case "$copies" in ''|*[!0-9]*) die "--copies takes a whole number" ;; esac
case "$draws" in ''|*[!0-9]*) die "--draws takes a whole number" ;; esac
# As numbers (a leading 0 is not octal): 00 is no load.
clears=$((10#$clears)); copies=$((10#$copies)); draws=$((10#$draws))
case "$timeout" in ''|*[!0-9]*) die "--timeout takes a whole number" ;; esac
case "$build" in
    release) build_args=(--locked --release -p victory); profile=release ;;
    diag) build_args=(--locked --profile diag -p victory --features diagnostics); profile=diag ;;
    *) die "--build takes release or diag, not '$build'" ;;
esac

cd "$(dirname "$0")/.."
root=$PWD
target_dir="${CARGO_TARGET_DIR:-target}"
case "$target_dir" in /*) ;; *) target_dir="$root/$target_dir" ;; esac
export CARGO_TARGET_DIR="$target_dir"
TARGET=x86_64-pc-windows-msvc
victory="$target_dir/$TARGET/$profile/victory.exe"
title_target="$target_dir/smoke-title"
title="$title_target/$TARGET/release/smoke-title.exe"

if [ "$do_build" = 1 ]; then
    scripts/build-windows-on-macos.sh "${build_args[@]}"
    # The fixture is not a workspace member: its own manifest and target dir.
    # (build-windows-on-macos.sh has checked the tools it needs.)
    (
        unset RUSTC RUSTUP_TOOLCHAIN
        brew_prefix="$(brew --prefix 2>/dev/null || echo /opt/homebrew)"
        export PATH="$brew_prefix/opt/llvm/bin:$brew_prefix/opt/lld/bin:$HOME/.cargo/bin:$PATH"
        cargo xwin build --quiet --release --target "$TARGET" \
            --manifest-path tests/fixtures/smoke-title/Cargo.toml --target-dir "$title_target"
    )
fi
[ -f "$victory" ] || die "no $victory (build it, or drop --no-build)"
[ -f "$title" ] || die "no $title (build it, or drop --no-build)"

# Wine as scripts/play-macos.sh finds it: WINE, else CrossOver in
# /Applications, then in ~/Applications.
crossover_wine=CrossOver.app/Contents/SharedSupport/CrossOver/bin/wine
crossover=/Applications/$crossover_wine
[ -x "$crossover" ] || crossover=$HOME/Applications/$crossover_wine
wine="${WINE:-$crossover}"
[ -x "$wine" ] || command -v "$wine" >/dev/null 2>&1 || die "no Wine at $wine (set WINE)"
case "$wine" in
    */CrossOver/bin/wine)
        where="bottle ${CX_BOTTLE:-victory}"
        wine_args=(--bottle "${CX_BOTTLE:-victory}") ;;
    *)
        export WINEPREFIX="${WINEPREFIX:-$HOME/.victory-wine}"
        where="prefix $WINEPREFIX"
        wine_args=() ;;
esac
# Rosetta reports AVX2 (which victory checks for) only when asked.
export ROSETTA_ADVERTISE_AVX="${ROSETTA_ADVERTISE_AVX:-1}"

# The run's own folder: the title (its folder is the package, G:), and
# VIctory's cache and data, so that nothing of other runs is read or kept.
work="$(mktemp -d "${TMPDIR:-/tmp}/victory-smoke.XXXXXX")"
cp "$title" "$work/smoke-title.exe"
mkdir -p "$work/cache" "$work/data"
# Wine's Z: is the Mac's /.
windows_path() {
    local p
    p=$(printf '%s' "$1" | tr -s /)
    printf 'Z:%s' "${p//\//\\}"
}
log="$work/run.log"

# None of the developer's switches (only those given with --env): the run as it is by default.
for key in $(env | sed -n 's/^\(VICTORY_[A-Za-z0-9_]*\)=.*/\1/p'); do unset "$key"; done
env_args=(VICTORY_LOG=info VICTORY_CACHE_DIR="$(windows_path "$work/cache")" VICTORY_DATA_DIR="$(windows_path "$work/data")")
[ "$unpaced" = 1 ] && env_args+=(VICTORY_FRAME_INTERVAL_MS=0)
env_args+=(${switches[@]+"${switches[@]}"})
title_args=("$frames" "$exit_code")
[ "$clears" = 0 ] && [ "$copies" = 0 ] && [ "$draws" = 0 ] || title_args+=("$clears" "$copies")
[ "$draws" = 0 ] || title_args+=("$draws")
echo "smoke-macos: $frames frames$([ "$unpaced" = 1 ] && echo ', unpaced')$([ "$clears" = 0 ] || echo ", $clears more clears a frame")$([ "$copies" = 0 ] || echo ", $copies copies a frame")$([ "$draws" = 0 ] || echo ", $draws draws a frame"), $where, log $log"
started=$(date +%s)
env "${env_args[@]}" "$wine" ${wine_args[@]+"${wine_args[@]}"} "$victory" run "$(windows_path "$work/smoke-title.exe")" --execute -- "${title_args[@]}" >"$log" 2>&1 &
pid=$!
timed_out=0
while kill -0 "$pid" 2>/dev/null; do
    if [ $(( $(date +%s) - started )) -ge "$timeout" ]; then
        timed_out=1
        kill "$pid" 2>/dev/null || true
        # Only this run's victory.exe: its command line names this run's
        # folder (victory-smoke.XXXXXX, unique).
        pkill -f "$(basename "$work")" 2>/dev/null || true
        break
    fi
    sleep 1
done
status=0
wait "$pid" 2>/dev/null || status=$?
elapsed=$(( $(date +%s) - started ))

clean="$work/run.txt"
sed $'s/\x1b\\[[0-9;]*m//g' "$log" >"$clean"
fail() { echo "smoke-macos: FAILED: $*" >&2; tail -n 30 "$clean" >&2; exit 1; }
[ "$timed_out" = 0 ] || fail "the run took more than ${timeout}s"
[ "$status" = 0 ] || fail "victory exited with status $status"
exited="$(sed -n 's/^title exited with //p' "$clean" | tail -n 1)"
[ -n "$exited" ] || fail "no \"title exited with\" line"
[ "$exited" = 0 ] || fail "title exited with $exited"
for why in "no host D3D12 device" "no swap chain"; do
    ! grep -q "$why" "$clean" || fail "$why"
done
times="$(sed -n 's/.* fps, \([0-9.]*\) ms\/frame (.*/\1/p' "$clean")"
[ -n "$times" ] || fail "no \"ms/frame\" line from the frame meter"
count=$(printf '%s\n' "$times" | wc -l | tr -d ' ')
median_of() { sort -n | awk '{ v[NR] = $1 } END { if (NR % 2) print v[(NR + 1) / 2]; else printf "%.2f\n", (v[NR / 2] + v[NR / 2 + 1]) / 2 }'; }
median=$(printf '%s\n' "$times" | median_of)
# Each meter line's draws run on the host per frame, and its split of the
# frame: CPU preparing the submissions, waiting for the GPU.
per_frame="$(sed -n 's/.* \([0-9][0-9]*\) draws\/frame, dropped .*/\1/p' "$clean")"
preparing="$(sed -n 's/.* ms\/frame (\([0-9.]*\) preparing: .*/\1/p' "$clean")"
on_gpu="$(sed -n 's/.*; \([0-9.]*\) on the GPU; .*/\1/p' "$clean")"
if [ "$draws" != 0 ]; then
    [ -n "$per_frame" ] || fail "no \"draws/frame\" in the frame meter's lines"
    ! printf '%s\n' "$per_frame" | grep -qx 0 || fail "a meter line with 0 draws/frame ($(printf '%s ' $per_frame)): the draws did not reach the host"
fi
split=
[ -z "$preparing" ] || split=", median $(printf '%s\n' "$preparing" | median_of) ms preparing, $(printf '%s\n' "$on_gpu" | median_of) ms on the GPU"
[ -z "$per_frame" ] || split="$split, median $(printf '%s\n' "$per_frame" | median_of) draws/frame"
echo "smoke-macos: PASSED in ${elapsed}s: title exited with 0, $count meter lines ($(printf '%s ' $times)ms/frame), median $median ms/frame$split"
