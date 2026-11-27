#!/bin/bash
# The launchd agent's job (scripts/launchd/com.victory.verify-macos.plist):
# fetches origin and, when origin/port-macos is not the commit verified last,
# runs scripts/verify-macos.sh --ref <that commit>, keeps its log and posts a
# macOS notification with the result. Posts nothing anywhere else.
#   scripts/verify-agent-run.sh [--force]
# --force   verify even if origin/port-macos has not moved
# Environment: VERIFY_STATE_DIR (default ~/Library/Application Support/victory-verify):
# `last` (the sha verified last), `last-result`, `logs/` (one folder per run).
# VERIFY_BRANCH (default port-macos). XWIN_ACCEPT_LICENSE=1, as verify-macos.sh needs it.
# Exit status: 0 nothing to do or passed, 1 failed, 2 could not run.
set -uo pipefail

force=0
case "${1:-}" in --force) force=1 ;; '') ;; *) echo "usage: $0 [--force]" >&2; exit 2 ;; esac
# launchd's PATH is /usr/bin:/bin:/usr/sbin:/sbin.
export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"

script_dir="$(cd "$(dirname "$0")" && pwd)"
repo="$(git -C "$script_dir" rev-parse --show-toplevel)" || exit 2
state="${VERIFY_STATE_DIR:-$HOME/Library/Application Support/victory-verify}"
branch="${VERIFY_BRANCH:-port-macos}"
mkdir -p "$state/logs" || exit 2
stamp() { date '+%Y-%m-%d %H:%M:%S'; }
notify() {
    # $1 title, $2 message; quotes cannot break out of the AppleScript strings.
    local t m
    t=$(printf '%s' "$1" | tr -d '"\\'); m=$(printf '%s' "$2" | tr -d '"\\')
    osascript -e "display notification \"$m\" with title \"$t\"" >/dev/null 2>&1 || true
}

# One run at a time (a run by hand and the agent's).
lock="$state/lock"
if ! mkdir "$lock" 2>/dev/null; then
    echo "$(stamp) another run holds $lock; nothing done"
    exit 0
fi
trap 'rmdir "$lock" 2>/dev/null' EXIT

if ! git -C "$repo" fetch --quiet origin "+refs/heads/$branch:refs/remotes/origin/$branch"; then
    echo "$(stamp) git fetch origin failed"
    exit 2
fi
sha="$(git -C "$repo" rev-parse --verify --quiet "origin/$branch^{commit}")" || { echo "$(stamp) no origin/$branch"; exit 2; }
last="$(cat "$state/last" 2>/dev/null || true)"
if [ "$force" = 0 ] && [ "$sha" = "$last" ]; then
    echo "$(stamp) origin/$branch is still ${sha:0:10}, verified last; nothing to do"
    exit 0
fi

log_dir="$state/logs/$(date +%Y%m%d-%H%M%S)-${sha:0:10}"
echo "$(stamp) verifying origin/$branch ${sha:0:10} (last verified: ${last:-none}); logs $log_dir"
status=0
"$script_dir/verify-macos.sh" --ref "$sha" --log-dir "$log_dir" >"$log_dir.log" 2>&1 || status=$?
mkdir -p "$log_dir"
mv "$log_dir.log" "$log_dir/verify.log" 2>/dev/null || true
# A pass is a status of 0 and the summary's PASSED line.
if [ "$status" = 0 ] && ! grep -q '^verify-macos: PASSED' "$log_dir/verify.log"; then status=2; fi
case "$status" in
    0) result=passed ;;
    1) result=failed ;;
    *) result="could not run (exit $status)" ;;
esac
echo "$(stamp) ${sha:0:10}: $result"
tail -n 40 "$log_dir/verify.log"
# A run that could not start (no Wine, no network...) is tried again next time.
if [ "$status" = 0 ] || [ "$status" = 1 ]; then
    printf '%s\n' "$sha" >"$state/last"
    printf '%s %s %s\n' "$sha" "$result" "$log_dir" >"$state/last-result"
fi
detail="$(sed -En 's/^verify-macos: (PASSED|FAILED) \(([^;]*);.*/\2/p' "$log_dir/verify.log" | tail -n 1)"
notify "VIctory verify: ${sha:0:10} $result" "origin/$branch${detail:+, $detail}. Log: $log_dir"
[ "$status" -le 2 ] && exit "$status" || exit 2
