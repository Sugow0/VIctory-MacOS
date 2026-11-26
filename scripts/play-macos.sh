#!/bin/bash
# Starts the title on macOS (Apple silicon) through Wine, Direct3D 12 translated
# by Apple's D3DMetal (Apple's tap's Wine + GPTK 2+'s D3DMetal, or CrossOver),
# with the settings scripts/play.ps1 plays with (docs/macos.md).
#   scripts/play-macos.sh --exe <path of your own copy's executable>
# --safe           without the newer, faster ways (the first thing to try when something is wrong)
# --ray-tracing    the title's own ray-traced indirect light: lit interiors, about a third slower
# --build B        diag (the default, the build all measurements are of) or release
# --more K=V       other settings, repeatable: --more VICTORY_PANEL=1
# --dry-run        prints the environment and the command, starts nothing
# Environment: WINE (the wine to use), WINEPREFIX (default ~/.victory-wine),
# CX_BOTTLE (CrossOver's bottle, required with CrossOver).
# Written for macOS's own /bin/bash (3.2): no associative arrays.
set -euo pipefail

usage() { sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'; }
die() { echo "play-macos: $*" >&2; exit 1; }

exe=''
safe=0
ray_tracing=0
build=diag
dry_run=0
more_keys=()
more_vals=()
while [ $# -gt 0 ]; do
    case "$1" in
        --exe) [ $# -ge 2 ] || die "--exe needs a path"; exe=$2; shift 2 ;;
        --exe=*) exe=${1#--exe=}; shift ;;
        --safe) safe=1; shift ;;
        --ray-tracing) ray_tracing=1; shift ;;
        --build) [ $# -ge 2 ] || die "--build needs diag or release"; build=$2; shift 2 ;;
        --build=*) build=${1#--build=}; shift ;;
        --more) [ $# -ge 2 ] || die "--more needs KEY=VALUE"; kv=$2; shift 2
            key=${kv%%=*}
            case "$kv" in *=*) ;; *) die "--more takes KEY=VALUE, not '$kv'" ;; esac
            case "$key" in ''|[0-9]*|*[!A-Za-z0-9_]*) die "--more: '$key' is not a variable name ([A-Za-z_][A-Za-z0-9_]*)" ;; esac
            more_keys+=("$key"); more_vals+=("${kv#*=}") ;;
        --dry-run) dry_run=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown argument '$1'" ;;
    esac
done
[ -n "$exe" ] || { usage >&2; die "--exe <path of your copy's executable> is required"; }
case "$build" in diag|release) ;; *) die "--build takes diag or release, not '$build'" ;; esac

# The settings, as play.ps1 has them (two arrays: bash 3.2 has no maps).
keys=()
vals=()
set_setting() {
    local i
    for i in "${!keys[@]}"; do
        if [ "${keys[$i]}" = "$1" ]; then vals[$i]=$2; return; fi
    done
    keys+=("$1"); vals+=("$2")
}
set_setting VICTORY_TEXTURE_NO_WAIT 1; set_setting VICTORY_RELAY 2; set_setting VICTORY_WIDE_BACK 1
set_setting VICTORY_CARD_REHASH_RUNS 1; set_setting VICTORY_JOIN_COPIES 1; set_setting VICTORY_INDIRECT_WINDOW 1
set_setting VICTORY_RELAY_COPY 1; set_setting VICTORY_CARD_STEPS 2; set_setting VICTORY_INDIRECT_BATCH 2
set_setting VICTORY_MAP_THREADS 1; set_setting VICTORY_TITLE_PRIORITIES 0; set_setting VICTORY_CARD_AWAKE 1
set_setting VICTORY_SHOW_AFTER 1; set_setting VICTORY_AMBIENT 0.8; set_setting VICTORY_D3D12_DRED light
if [ "$safe" = 1 ]; then
    set_setting VICTORY_UNDER_WIDE 0; set_setting VICTORY_LAND_ECHO 0
else
    set_setting VICTORY_THIN 1; set_setting VICTORY_JOIN_PARTS 8; set_setting VICTORY_UPLOAD_AHEAD 1
fi
if [ "$ray_tracing" = 1 ]; then set_setting VICTORY_BVH_BUILDER 1; fi
for i in "${!more_keys[@]}"; do set_setting "${more_keys[$i]}" "${more_vals[$i]}"; done

# --- The host -------------------------------------------------------------
[ "$(uname -s)" = Darwin ] || die "this launcher is for macOS; on Windows use scripts\\play.ps1"
[ "$(uname -m)" = arm64 ] || die "D3DMetal needs a Mac with Apple silicon (this one is $(uname -m))"
arch -x86_64 /usr/bin/true 2>/dev/null \
    || die "Rosetta 2 is not installed: softwareupdate --install-rosetta --agree-to-license"
macos=$(sw_vers -productVersion)
# Rosetta's AVX2 (which victory.exe is built for): the sources disagree.
# AppleGamingWiki has it from macOS 15.0; Apple's developer forums
# (https://developer.apple.com/forums/thread/769486) from 15.4, and only
# reported by cpuid with ROSETTA_ADVERTISE_AVX=1. victory checks AVX2 by
# cpuid, so it is asked for. Untested here.
macos_major=${macos%%.*}
macos_minor=$(printf '%s' "$macos" | cut -s -d. -f2)
case "$macos_minor" in ''|*[!0-9]*) macos_minor=0 ;; esac
[ "$macos_major" -ge 15 ] || die "macOS 15 or later is needed (Rosetta's AVX2); this is $macos"
if [ "$macos_major" -eq 15 ] && [ "$macos_minor" -lt 4 ]; then
    echo "play-macos: warning: macOS $macos; Rosetta may report AVX2 only from 15.4 on" >&2
fi
export ROSETTA_ADVERTISE_AVX="${ROSETTA_ADVERTISE_AVX:-1}"

repo=$(cd "$(dirname "$0")/.." && pwd)
# Built on the Mac (cross target), or copied from a Windows build.
victory="$repo/target/x86_64-pc-windows-msvc/$build/victory.exe"
[ -f "$victory" ] || [ ! -f "$repo/target/$build/victory.exe" ] || victory="$repo/target/$build/victory.exe"
[ -f "$victory" ] || die "No $victory. Build it first (docs/macos.md)."
[ -f "$exe" ] || die "No $exe."
exe="$(cd "$(dirname "$exe")" && pwd)/$(basename "$exe")"

# A Unix path as Windows sees it: Wine's Z: drive is the Unix root.
to_windows() {
    local p
    p=$(printf '%s' "$1" | tr -s /)
    printf 'Z:%s' "${p//\//\\}"
}

# --- Wine -----------------------------------------------------------------
kind=''
wine=''
gptk=/usr/local/opt/game-porting-toolkit/bin/wine64
# CrossOver in /Applications first, then in the user's own ~/Applications.
crossover_wine=CrossOver.app/Contents/SharedSupport/CrossOver/bin/wine
crossover=/Applications/$crossover_wine
[ -x "$crossover" ] || crossover=$HOME/Applications/$crossover_wine
if [ -n "${WINE:-}" ]; then
    kind=env; wine=$WINE
    [ -x "$wine" ] || command -v "$wine" >/dev/null 2>&1 || die "WINE=$wine is not an executable"
elif [ -x "$gptk" ]; then
    # Apple's tap (formula game-porting-toolkit, Wine of crossover-sources
    # 22.1.1); D3DMetal comes from GPTK 2+, copied in by hand.
    kind=gptk; wine=$gptk
elif command -v wine64 >/dev/null 2>&1; then
    kind=wine; wine=$(command -v wine64)
    echo "play-macos: warning: $wine is a wine64 found on PATH; whether it has D3DMetal is not checked" >&2
elif [ -x "$crossover" ]; then
    kind=crossover; wine=$crossover
else
    die "no Wine with D3DMetal found. Install a Wine built from Apple's tap (/usr/local/opt/game-porting-toolkit/bin/wine64) with the D3DMetal libraries of Game Porting Toolkit 2 or later copied in, or CrossOver 24 or later in /Applications or ~/Applications, or set WINE to a wine binary (docs/macos.md)."
fi
echo "play-macos: wine ($kind): $wine" >&2
wine_args=()
wine_env=()
if [ "$kind" = crossover ]; then
    # CrossOver keeps its prefixes as bottles; WINEPREFIX is not its way.
    # D3DMetal is a per-bottle setting, so the bottle is named on purpose
    # rather than left to CrossOver's default one.
    [ -n "${CX_BOTTLE:-}" ] || die "with CrossOver, set CX_BOTTLE to a bottle with D3DMetal turned on in its settings (docs/macos.md)"
    wine_args=(--bottle "$CX_BOTTLE")
else
    export WINEPREFIX="${WINEPREFIX:-$HOME/.victory-wine}"
    wine_env+=("WINEPREFIX=$WINEPREFIX")
fi
if [ "$kind" = gptk ] && [ -z "${WINEESYNC+x}" ]; then
    # Apple's Game Porting Toolkit 1 read-me starts games with WINEESYNC=1
    # (eventfd-based synchronization, fewer wineserver round trips). CrossOver
    # sets its sync per bottle, so it is left alone there; WINEMSYNC is not
    # set, since nothing documents it for either.
    export WINEESYNC=1
fi
if [ -n "${WINEESYNC+x}" ]; then wine_env+=("WINEESYNC=$WINEESYNC"); fi
wine_env+=("ROSETTA_ADVERTISE_AVX=$ROSETTA_ADVERTISE_AVX")

# --- The environment, as play.ps1 leaves it --------------------------------
# (Other settings left over in this shell go; where your files are stays.)
paths=' VICTORY_CONFIG VICTORY_GAME_DIR VICTORY_CACHE_DIR VICTORY_DATA_DIR '
in_list() { case " $2 " in *" $1 "*) return 0 ;; *) return 1 ;; esac; }
inherited_keys=()
inherited_vals=()
for name in $(compgen -e); do
    case "$name" in VICTORY_*) ;; *) continue ;; esac
    in_list "$name" "$paths" && continue
    inherited_keys+=("$name"); inherited_vals+=("${!name}")
    unset "$name"
done
for i in "${!keys[@]}"; do export "${keys[$i]}=${vals[$i]}"; done
discarded=()
for i in "${!inherited_keys[@]}"; do
    name=${inherited_keys[$i]}
    in_list "$name" " ${more_keys[*]:-} " && continue
    if [ -z "${!name+x}" ] || [ "${!name}" != "${inherited_vals[$i]}" ]; then discarded+=("$name"); fi
done
if [ ${#discarded[@]} -gt 0 ]; then
    echo "Discarded inherited values for $(printf '%s\n' "${discarded[@]}" | sort | paste -sd, - | sed 's/,/, /g'). Use --more to pass them explicitly."
fi
# Paths given the Unix way are handed to Windows the Windows way.
path_env=()
for name in $paths; do
    if [ -n "${!name:-}" ]; then
        case "${!name}" in /*) export "$name=$(to_windows "${!name}")" ;; esac
        path_env+=("$name=${!name}")
    fi
done

command=("$wine")
if [ ${#wine_args[@]} -gt 0 ]; then command+=("${wine_args[@]}"); fi
command+=("$victory" run "$(to_windows "$exe")" --execute)

if [ "$dry_run" = 1 ]; then
    for e in ${wine_env[@]+"${wine_env[@]}"} ${path_env[@]+"${path_env[@]}"}; do printf 'export %q\n' "$e"; done
    for i in "${!keys[@]}"; do
        in_list "${keys[$i]}" "$paths" && continue   # shown above, as Windows gets them
        printf 'export %q\n' "${keys[$i]}=${vals[$i]}"
    done
    if [ "$kind" != crossover ] && [ ! -f "$WINEPREFIX/system.reg" ]; then
        printf '%q wineboot --init\n' "$wine"
    fi
    printf 'cd %q\n' "$repo"
    printf '%q ' "${command[@]}"; echo
    exit 0
fi

if [ "$kind" != crossover ] && [ ! -f "$WINEPREFIX/system.reg" ]; then
    echo "Making the Wine prefix $WINEPREFIX (once)."
    mkdir -p "$WINEPREFIX"
    "$wine" wineboot --init
fi
cd "$repo"   # victory.toml is looked for here
exec "${command[@]}"
