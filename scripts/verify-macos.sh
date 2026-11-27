#!/bin/bash
# Verifies a commit on this Mac: the workspace's tests natively, the same
# tests cross-built for Windows and run under Wine (CrossOver's bottle, D3D12
# by D3DMetal), then scripts/smoke-macos.sh. Prints a table per test binary
# and exits non-zero on any failure not in the allowlist below.
#   XWIN_ACCEPT_LICENSE=1 scripts/verify-macos.sh [--ref R] [--skip-native] [--skip-wine] [--skip-smoke] [--timeout S] [--log-dir D]
# --ref R         verify commit R (any git ref) in a temporary worktree, not this checkout
# --skip-native   no native `cargo test`
# --skip-wine     no cross-built tests under Wine
# --skip-smoke    no scripts/smoke-macos.sh
# --timeout S     seconds each Windows test binary may run (default 900)
# --log-dir D     where the logs go (default ~/Library/Logs/victory-verify/<date>-<sha>)
# Environment: XWIN_ACCEPT_LICENSE=1 (required for the Windows builds: you
# accept the Microsoft CRT/SDK license cargo-xwin downloads), CX_BOTTLE
# (default victory), WINE (default CrossOver's), VERIFY_CACHE_DIR (cargo
# target dirs kept between runs; default ~/Library/Caches/victory-verify).
# Exit status: 0 passed, 1 a test or stage failed, 2 could not run.
# Written for macOS's own /bin/bash (3.2): no associative arrays, no mapfile.
set -euo pipefail
export LC_ALL=C

# Failures under Wine that come from the bottle, not from the code, as
# "<package> <binary> <test name>" (exact). Anything else fails the run.
# - xtask's repository_passes_policy and documented_switches_are_read list
#   the repository's files with `git ls-files`: the bottle has no git.exe.
# - victory-loader's load_probe and victory's smoke_title end-to-end tests
#   build their fixture by running cargo (env!("CARGO"), the Mac's path)
#   from inside Wine, which cannot start a Mac binary. smoke-macos.sh runs
#   the smoke title end to end instead.
ALLOWED_WINE_FAILURES='
xtask xtask policy::tests::repository_passes_policy
xtask xtask switches::documented_switches_are_read
victory-loader load_probe loads_binds_and_calls_probe
victory smoke_title end_to_end::a_synthetic_title_presents_frames_and_exits_0
'

usage() { sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'; }
die() { echo "verify-macos: $*" >&2; exit 2; }
say() { echo "verify-macos: $*"; }

ref=''
do_native=1
do_wine=1
do_smoke=1
timeout=900
log_dir=''
while [ $# -gt 0 ]; do
    case "$1" in
        --ref) [ $# -ge 2 ] || die "--ref needs a git ref"; ref=$2; shift 2 ;;
        --ref=*) ref=${1#--ref=}; shift ;;
        --skip-native) do_native=0; shift ;;
        --skip-wine) do_wine=0; shift ;;
        --skip-smoke) do_smoke=0; shift ;;
        --timeout) [ $# -ge 2 ] || die "--timeout needs seconds"; timeout=$2; shift 2 ;;
        --log-dir) [ $# -ge 2 ] || die "--log-dir needs a path"; log_dir=$2; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown argument '$1'" ;;
    esac
done
case "$timeout" in ''|*[!0-9]*) die "--timeout takes a whole number" ;; esac
[ "$(uname -s)" = Darwin ] || die "this script is for macOS"
if [ "$do_wine" = 1 ] || [ "$do_smoke" = 1 ]; then
    [ "${XWIN_ACCEPT_LICENSE:-}" = 1 ] || die "the Windows builds need XWIN_ACCEPT_LICENSE=1: cargo-xwin downloads the Microsoft CRT and Windows SDK, under Microsoft's license (https://github.com/Jake-Shadle/xwin); set it to accept, or pass --skip-wine --skip-smoke"
fi

script_dir="$(cd "$(dirname "$0")" && pwd)"
repo="$(git -C "$script_dir" rev-parse --show-toplevel)"
worktree=''
cleanup() {
    if [ -n "$worktree" ]; then
        git -C "$repo" worktree remove --force "$worktree" >/dev/null 2>&1 || rm -rf "$worktree"
        git -C "$repo" worktree prune >/dev/null 2>&1 || true
    fi
}
trap cleanup EXIT
if [ -n "$ref" ]; then
    sha="$(git -C "$repo" rev-parse --verify --quiet "$ref^{commit}")" || die "no commit '$ref'"
    worktree="$(mktemp -d "${TMPDIR:-/tmp}/victory-verify.XXXXXX")"
    git -C "$repo" worktree add --detach --quiet "$worktree" "$sha" || die "git worktree add failed"
    src=$worktree
else
    src=$repo
    sha="$(git -C "$src" rev-parse HEAD)"
fi
short=${sha:0:10}
dirty=''
[ -z "$(git -C "$src" status --porcelain --untracked-files=no)" ] || dirty=' (with uncommitted changes)'

[ -n "$log_dir" ] || log_dir="$HOME/Library/Logs/victory-verify/$(date +%Y%m%d-%H%M%S)-$short"
mkdir -p "$log_dir"
log_dir="$(cd "$log_dir" && pwd)"
cache="${VERIFY_CACHE_DIR:-$HOME/Library/Caches/victory-verify}"
mkdir -p "$cache"
rows="$log_dir/rows.tsv"
: >"$rows"
say "verifying $short$dirty in $src"
say "logs in $log_dir"

# One row per test binary: stage, binary, passed, failed, ignored, status.
row() { printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$@" >>"$rows"; }
is_allowed() {
    printf '%s\n' "$ALLOWED_WINE_FAILURES" | grep -qxF "$1"
}

# The toolchain rust-toolchain.toml pins, by rustup's path: Homebrew's cargo,
# often first on PATH, ignores the file.
channel="$(sed -n 's/^channel *= *"\(.*\)"/\1/p' "$src/rust-toolchain.toml" 2>/dev/null | head -n 1)"
[ -n "$channel" ] || channel=stable
host="$( [ "$(uname -m)" = arm64 ] && echo aarch64 || echo x86_64 )-apple-darwin"

# --- Native ---------------------------------------------------------------
if [ "$do_native" = 1 ]; then
    T="$HOME/.rustup/toolchains/$channel-$host/bin"
    [ -x "$T/cargo" ] || die "no toolchain $channel-$host in ~/.rustup (rustup toolchain install $channel)"
    say "native: cargo test --workspace --locked ($channel-$host)"
    status=0
    ( cd "$src" && unset RUSTUP_TOOLCHAIN && PATH="$T:$PATH" RUSTC="$T/rustc" CARGO_TARGET_DIR="$cache/native" \
        "$T/cargo" test --workspace --locked --no-fail-fast ) >"$log_dir/native.log" 2>&1 || status=$?
    # "Running <src> (<path>/deps/<crate>-<hash>)" or "Doc-tests <crate>",
    # then that binary's "test result:" and its "... FAILED" lines.
    awk -v rows="$rows" '
        function flush(st) {
            if (name == "") return
            if (!seen) { st = "CRASHED" } else if (failed > 0) { st = "FAILED: " list } else { st = "ok" }
            printf "native\t%s\t%s\t%s\t%s\t%s\n", name, passed, failed, ignored, st >> rows
            name = ""
        }
        /^ *Running / {
            flush(); p = $NF; gsub(/[()]/, "", p); n = split(p, parts, "/"); b = parts[n]; sub(/-[0-9a-f]+$/, "", b)
            name = b " (" $2 ")"; seen = 0; passed = failed = ignored = 0; list = ""; next
        }
        /^ *Doc-tests / { flush(); name = $2 " (doc-tests)"; seen = 0; passed = failed = ignored = 0; list = ""; next }
        /^test .* \.\.\. FAILED$/ { t = $0; sub(/^test /, "", t); sub(/ \.\.\. FAILED$/, "", t); list = list (list == "" ? "" : ", ") t }
        /^test result:/ {
            seen = 1
            for (i = 1; i <= NF; i++) {
                if ($(i + 1) ~ /^passed;?$/) passed = $i
                if ($(i + 1) ~ /^failed;?$/) failed = $i
                if ($(i + 1) ~ /^ignored;?$/) ignored = $i
            }
        }
        END { flush() }
    ' "$log_dir/native.log"
    if [ "$status" != 0 ] && ! grep -q "	FAILED\|	CRASHED" "$rows"; then
        row native "cargo test (build)" - - - "FAILED: cargo exited $status before any test failed, see native.log"
    fi
fi

# --- Wine: the tests cross-built for Windows -------------------------------
crossover_wine=CrossOver.app/Contents/SharedSupport/CrossOver/bin/wine
crossover=/Applications/$crossover_wine
[ -x "$crossover" ] || crossover=$HOME/Applications/$crossover_wine
wine="${WINE:-$crossover}"
bottle="${CX_BOTTLE:-victory}"
if [ "$do_wine" = 1 ]; then
    [ -x "$wine" ] || die "no Wine at $wine (set WINE)"
    case "$wine" in
        */CrossOver/bin/wine) wine_args=(--bottle "$bottle") ;;
        *) export WINEPREFIX="${WINEPREFIX:-$HOME/.victory-wine}"; wine_args=() ;;
    esac
    brew_prefix="$(brew --prefix 2>/dev/null || echo /opt/homebrew)"
    say "wine: cargo xwin test --no-run --target x86_64-pc-windows-msvc --workspace"
    status=0
    ( cd "$src" && unset RUSTC RUSTUP_TOOLCHAIN \
        && PATH="$brew_prefix/opt/llvm/bin:$brew_prefix/opt/lld/bin:$HOME/.cargo/bin:$PATH" CARGO_TARGET_DIR="$cache/xwin" \
        cargo xwin test --no-run --locked --target x86_64-pc-windows-msvc --workspace --message-format=json \
    ) >"$log_dir/xwin-build.json" 2>"$log_dir/xwin-build.log" || status=$?
    if [ "$status" != 0 ]; then
        row wine "cargo xwin test --no-run" - - - "FAILED: build exited $status, see xwin-build.log"
    else
        # package, target name, package folder, exe (tab-separated).
        python3 -I -c '
import json, os, sys
for line in open(sys.argv[1], encoding="utf-8"):
    line = line.strip()
    if not line.startswith("{"):
        continue
    m = json.loads(line)
    if m.get("reason") != "compiler-artifact" or not m.get("profile", {}).get("test") or not m.get("executable"):
        continue
    pkg = m["package_id"]
    # "path+file:///.../crates/victory-pe#0.1.0" or "victory-pe 0.1.0 (path+...)"
    name = pkg.split()[0] if " " in pkg else pkg.rsplit("/", 1)[-1].split("#")[0].split("@")[0]
    print("\t".join([name, m["target"]["name"], os.path.dirname(m["manifest_path"]), m["executable"]]))
' "$log_dir/xwin-build.json" >"$log_dir/xwin-exes.tsv"
        [ -s "$log_dir/xwin-exes.tsv" ] || row wine "cargo xwin test --no-run" - - - "FAILED: no test executables in the build's output"
        export ROSETTA_ADVERTISE_AVX="${ROSETTA_ADVERTISE_AVX:-1}"
        say "wine: $(wc -l <"$log_dir/xwin-exes.tsv" | tr -d ' ') test binaries, bottle $bottle, ${timeout}s each"
        mkdir -p "$log_dir/wine"
        while IFS="	" read -r pkg target dir exe; do
            label="$pkg $target"
            out="$log_dir/wine/$pkg-$target"
            started=$(date +%s)
            # As cargo test runs them: from the package's folder.
            ( cd "$dir" && exec "$wine" ${wine_args[@]+"${wine_args[@]}"} "$exe" --test-threads=1 ) \
                >"$out.out" 2>"$out.err" </dev/null &
            pid=$!
            timed_out=0
            while kill -0 "$pid" 2>/dev/null; do
                if [ $(( $(date +%s) - started )) -ge "$timeout" ]; then
                    timed_out=1
                    kill "$pid" 2>/dev/null || true
                    # The Windows process itself: its path is unique (cargo's hash).
                    pkill -f "$exe" 2>/dev/null || true
                    break
                fi
                sleep 1
            done
            st=0
            wait "$pid" 2>/dev/null || st=$?
            tr -d '\r' <"$out.out" >"$out.txt"
            result="$(grep '^test result:' "$out.txt" | tail -n 1 || true)"
            num() { printf '%s\n' "$result" | sed -n "s/.* \([0-9][0-9]*\) $1.*/\1/p"; }
            passed="$(num passed)"; failed="$(num failed)"; ignored="$(num ignored)"
            if [ "$timed_out" = 1 ]; then
                row wine "$label" "${passed:--}" "${failed:--}" "${ignored:--}" "TIMEOUT after ${timeout}s"
                continue
            fi
            if [ -z "$result" ]; then
                row wine "$label" - - - "CRASHED: exit $st, no test result (see wine/$pkg-$target.err)"
                continue
            fi
            bad=''; allowed=0
            while IFS= read -r t; do
                [ -n "$t" ] || continue
                if is_allowed "$label $t"; then allowed=$((allowed + 1)); else bad="$bad${bad:+, }$t"; fi
            done <<EOF
$(sed -n 's/^test \(.*\) \.\.\. FAILED$/\1/p' "$out.txt")
EOF
            if [ -n "$bad" ]; then
                row wine "$label" "$passed" "$failed" "$ignored" "FAILED: $bad"
            elif [ "$allowed" != "${failed:-0}" ] || { [ "$st" != 0 ] && [ "$allowed" = 0 ]; }; then
                row wine "$label" "$passed" "$failed" "$ignored" "FAILED: exit $st, $failed failed, $allowed allowlisted"
            elif [ "$allowed" -gt 0 ]; then
                row wine "$label" "$passed" "$failed" "$ignored" "ok ($allowed allowlisted)"
            else
                row wine "$label" "$passed" "$failed" "$ignored" ok
            fi
        done <"$log_dir/xwin-exes.tsv"
    fi
fi

# --- Smoke ----------------------------------------------------------------
if [ "$do_smoke" = 1 ]; then
    say "smoke: scripts/smoke-macos.sh"
    if [ ! -x "$src/scripts/smoke-macos.sh" ]; then
        row smoke smoke-macos.sh - - - "FAILED: no scripts/smoke-macos.sh in $short"
    else
        status=0
        CARGO_TARGET_DIR="$cache/smoke" "$src/scripts/smoke-macos.sh" >"$log_dir/smoke.log" 2>&1 </dev/null || status=$?
        if [ "$status" = 0 ]; then
            row smoke smoke-macos.sh 1 0 0 "ok: $(sed -n 's/^smoke-macos: PASSED in //p' "$log_dir/smoke.log" | tail -n 1)"
        else
            row smoke smoke-macos.sh 0 1 0 "FAILED: exit $status, see smoke.log"
        fi
    fi
fi

# --- Summary --------------------------------------------------------------
summary="$log_dir/summary.txt"
{
    echo "verify-macos: $short$dirty, $(date '+%Y-%m-%d %H:%M')"
    printf '%-7s %-48s %6s %6s %7s  %s\n' stage binary passed failed ignored status
    while IFS="	" read -r stage bin p f i st; do
        printf '%-7s %-48s %6s %6s %7s  %s\n' "$stage" "$bin" "$p" "$f" "$i" "$st"
    done <"$rows"
} >"$summary"
bad_rows="$(awk -F '\t' '$6 !~ /^ok/' "$rows" | wc -l | tr -d ' ')"
total_rows="$(wc -l <"$rows" | tr -d ' ')"
if [ "$total_rows" = 0 ]; then
    echo "verify-macos: nothing was run" >>"$summary"; cat "$summary"; exit 2
fi
if [ "$bad_rows" = 0 ]; then
    echo "verify-macos: PASSED ($total_rows binaries; logs $log_dir)" >>"$summary"
    cat "$summary"
    exit 0
fi
echo "verify-macos: FAILED ($bad_rows of $total_rows binaries; logs $log_dir)" >>"$summary"
cat "$summary"
exit 1
