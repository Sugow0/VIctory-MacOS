#!/bin/bash
# Installs (or removes) the launchd agent that verifies origin/port-macos on
# this Mac when it moves (scripts/verify-agent-run.sh, every N hours).
#   XWIN_ACCEPT_LICENSE=1 scripts/install-verify-agent.sh install [--hours N]
#   scripts/install-verify-agent.sh uninstall
#   scripts/install-verify-agent.sh status
# install     writes ~/Library/LaunchAgents/com.victory.verify-macos.plist and loads it;
#             the agent runs the scripts of this checkout: keep it, on port-macos
# --hours N   how often it looks (default 6)
# XWIN_ACCEPT_LICENSE=1 is required: the agent's Windows builds download the
# Microsoft CRT and SDK under Microsoft's license, and you accept it here.
# State and logs: ~/Library/Application Support/victory-verify (VERIFY_STATE_DIR).
set -euo pipefail

label=com.victory.verify-macos
plist="$HOME/Library/LaunchAgents/$label.plist"
script_dir="$(cd "$(dirname "$0")" && pwd)"
state="${VERIFY_STATE_DIR:-$HOME/Library/Application Support/victory-verify}"
domain="gui/$(id -u)"
usage() { sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'; }
die() { echo "install-verify-agent: $*" >&2; exit 1; }

cmd="${1:-}"; [ $# -gt 0 ] && shift
case "$cmd" in
    install)
        hours=6
        while [ $# -gt 0 ]; do
            case "$1" in
                --hours) [ $# -ge 2 ] || die "--hours needs a number"; hours=$2; shift 2 ;;
                *) usage >&2; die "unknown argument '$1'" ;;
            esac
        done
        case "$hours" in ''|*[!0-9]*|0) die "--hours takes a whole number above 0" ;; esac
        [ "${XWIN_ACCEPT_LICENSE:-}" = 1 ] || die "set XWIN_ACCEPT_LICENSE=1 to accept the Microsoft CRT/SDK license the agent's builds download (https://github.com/Jake-Shadle/xwin)"
        runner="$script_dir/verify-agent-run.sh"
        [ -x "$runner" ] || die "no $runner"
        mkdir -p "$state" "$(dirname "$plist")"
        # Placeholders are filled by python (paths may hold any character but NUL).
        python3 -I -c '
import sys
src, dst, runner, interval, state = sys.argv[1:6]
esc = lambda s: s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
t = open(src, encoding="utf-8").read()
for k, v in (("@RUNNER@", esc(runner)), ("<integer>21600</integer>", "<integer>%s</integer>" % interval), ("@STATE@", esc(state)), ("@XWIN_ACCEPT_LICENSE@", "1")):
    t = t.replace(k, v)
open(dst, "w", encoding="utf-8").write(t)
' "$script_dir/launchd/$label.plist" "$plist" "$runner" "$((hours * 3600))" "$state"
        plutil -lint "$plist" >/dev/null || die "the plist written is not valid: $plist"
        launchctl bootout "$domain/$label" 2>/dev/null || true
        launchctl bootstrap "$domain" "$plist"
        echo "installed: $plist, every ${hours}h, runs $runner"
        echo "log: $state/agent.log; run now: launchctl kickstart $domain/$label"
        ;;
    uninstall)
        [ $# -eq 0 ] || die "uninstall takes no arguments"
        launchctl bootout "$domain/$label" 2>/dev/null || true
        rm -f "$plist"
        echo "removed $plist (state and logs kept in $state)"
        ;;
    status)
        if launchctl print "$domain/$label" >/dev/null 2>&1; then echo "loaded: $plist"; else echo "not loaded"; fi
        [ -f "$state/last-result" ] && echo "last: $(cat "$state/last-result")" || true
        ;;
    -h|--help|'') usage ;;
    *) usage >&2; die "unknown command '$cmd'" ;;
esac
