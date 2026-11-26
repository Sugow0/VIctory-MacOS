#!/bin/bash
# Cross-compile victory.exe (x86_64-pc-windows-msvc) on macOS with cargo-xwin.
#
# Usage: scripts/build-windows-on-macos.sh [--accept-license] [cargo build args...]
#
# Without cargo args it builds: --locked --profile diag -p victory --features diagnostics
# Needs: rustup (toolchain pinned by rust-toolchain.toml), Homebrew `llvm` and `lld`.
# cargo-xwin downloads the Microsoft CRT and Windows SDK on first use; that needs
# their license accepted, with --accept-license or XWIN_ACCEPT_LICENSE=1.
set -euo pipefail

TARGET=x86_64-pc-windows-msvc
cd "$(dirname "$0")/.."

die() { echo "error: $*" >&2; exit 1; }

[ "$(uname -s)" = "Darwin" ] || die "this script is for macOS (Darwin) only"

accept_license=0
cargo_args=()
for arg in "$@"; do
    case "$arg" in
        --accept-license) accept_license=1 ;;
        *) cargo_args+=("$arg") ;;
    esac
done
if [ "${#cargo_args[@]}" -eq 0 ]; then
    cargo_args=(--locked --profile diag -p victory --features diagnostics)
fi

# rustup's cargo honors rust-toolchain.toml; Homebrew's cargo does not.
[ -x "$HOME/.cargo/bin/cargo" ] || die "rustup's cargo not found in ~/.cargo/bin (install rustup)"
export PATH="$HOME/.cargo/bin:$PATH"

# clang-cl, llvm-lib, llvm-ar, llvm-rc come from Homebrew llvm; lld-link from Homebrew lld.
brew_prefix="$(brew --prefix 2>/dev/null || echo /opt/homebrew)"
llvm_bin="$brew_prefix/opt/llvm/bin"
lld_bin="$brew_prefix/opt/lld/bin"
[ -x "$llvm_bin/clang-cl" ] || die "clang-cl not found in $llvm_bin (brew install llvm)"
[ -x "$lld_bin/lld-link" ] || die "lld-link not found in $lld_bin (brew install lld)"
export PATH="$llvm_bin:$lld_bin:$PATH"

if ! cargo xwin --version >/dev/null 2>&1; then
    die "cargo-xwin is missing: run  ~/.cargo/bin/cargo install --locked cargo-xwin"
fi

if [ "$accept_license" = 1 ]; then
    export XWIN_ACCEPT_LICENSE=1
fi
if [ "${XWIN_ACCEPT_LICENSE:-}" != 1 ]; then
    cat >&2 <<'EOF'
cargo-xwin downloads the Microsoft C runtime and Windows SDK, which are
under Microsoft's license terms (see https://github.com/Jake-Shadle/xwin).
Rerun with --accept-license (or set XWIN_ACCEPT_LICENSE=1) to accept them.
EOF
    exit 1
fi

rustup target list --installed | grep -qx "$TARGET" || rustup target add "$TARGET"

cargo xwin build --target "$TARGET" "${cargo_args[@]}"

# Report where the exe landed, from the profile in the args.
profile=debug
prev=
for arg in "${cargo_args[@]}"; do
    case "$arg" in
        --release) profile=release ;;
        --profile=*) profile="${arg#--profile=}" ;;
    esac
    [ "$prev" = "--profile" ] && profile="$arg"
    prev="$arg"
done
[ "$profile" = dev ] && profile=debug
exe="target/$TARGET/$profile/victory.exe"
if [ -f "$exe" ]; then
    echo "built: $(cd "$(dirname "$exe")" && pwd)/victory.exe"
else
    echo "build finished; no $exe (did the args select another package?)"
fi
