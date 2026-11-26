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
# Arguments replace the defaults; the target is always $TARGET.
if [ "${#cargo_args[@]}" -eq 0 ]; then
    cargo_args=(--locked --profile diag -p victory --features diagnostics)
fi

# Where the exe lands: profile and target dir, read from the args.
profile=debug
target_dir="${CARGO_TARGET_DIR:-target}"
prev=
for arg in "${cargo_args[@]}"; do
    # What follows `--` goes to the program, not to cargo.
    [ "$arg" = "--" ] && break
    case "$prev" in
        --profile) profile="$arg" ;;
        --target-dir) target_dir="$arg" ;;
    esac
    case "$arg" in
        -r|--release) profile=release ;;
        --profile=*) profile="${arg#--profile=}" ;;
        --target-dir=*) target_dir="${arg#--target-dir=}" ;;
        --target|--target=*) die "the target is always $TARGET; drop $arg" ;;
    esac
    prev="$arg"
done
# Cargo's folder for its built-in profiles: dev and test in debug/, bench in release/.
case "$profile" in dev|test) profile=debug ;; bench) profile=release ;; esac

# rustup's cargo honors rust-toolchain.toml; Homebrew's cargo does not.
[ -x "$HOME/.cargo/bin/cargo" ] || die "rustup's cargo not found in ~/.cargo/bin (install rustup)"
export PATH="$HOME/.cargo/bin:$PATH"
# Either would bypass rustup's pick of the toolchain pinned in rust-toolchain.toml.
unset RUSTC RUSTUP_TOOLCHAIN

# clang-cl, llvm-lib, llvm-ar, llvm-rc come from Homebrew llvm; lld-link from Homebrew lld.
brew_prefix="$(brew --prefix 2>/dev/null || echo /opt/homebrew)"
llvm_bin="$brew_prefix/opt/llvm/bin"
lld_bin="$brew_prefix/opt/lld/bin"
[ -x "$llvm_bin/clang-cl" ] || die "clang-cl not found in $llvm_bin (brew install llvm)"
[ -x "$lld_bin/lld-link" ] || die "lld-link not found in $lld_bin (brew install lld)"
export PATH="$llvm_bin:$lld_bin:$PATH"

if ! command -v cargo-xwin >/dev/null 2>&1; then
    die "cargo-xwin is missing: run  ~/.cargo/bin/cargo install --locked cargo-xwin"
fi
if ! xwin_version="$(cargo xwin --version 2>&1)"; then
    echo "$xwin_version" >&2
    die "cargo-xwin is installed but 'cargo xwin --version' failed (output above)"
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

# rust-toolchain.toml lists the target, so rustup installs it when needed.
cargo xwin build --target "$TARGET" "${cargo_args[@]}"

exe="$target_dir/$TARGET/$profile/victory.exe"
if [ -f "$exe" ]; then
    echo "built: $(cd "$(dirname "$exe")" && pwd)/victory.exe"
else
    echo "build finished; no $exe (did the args select another package?)"
fi
