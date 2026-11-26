#!/bin/bash
# Puts Microsoft's d3dcompiler_47.dll (x64) into the Wine prefix or CrossOver
# bottle and sets its DLL override to native: Wine's builtin d3dcompiler_47
# rejects some of the runtime's own HLSL shaders (issue #16, docs/macos.md).
#   scripts/macos-setup-bottle.sh [--bottle B] [--dry-run]
# --bottle B   CrossOver's bottle (same as CX_BOTTLE)
# --dry-run    prints what would be done, downloads and changes nothing
# Environment: WINE, WINEPREFIX (default ~/.victory-wine), CX_BOTTLE, as
# scripts/play-macos.sh reads them.
# Needs curl, bsdtar (macOS has both) and osslsigncode (brew install osslsigncode).
# Written for macOS's own /bin/bash (3.2).
set -euo pipefail

usage() { sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'; }
die() { echo "macos-setup-bottle: $*" >&2; exit 1; }
say() { echo "macos-setup-bottle: $*" >&2; }

# The Windows SDK 10.0.26100.9457 (September 2026 servicing), as Visual
# Studio's installer manifest lists it (package Win11SDK_10.0.26100, msi
# "Windows SDK for Windows Store Apps Tools-x86_en-us.msi"). The cab holds the
# DLL under its MSI file key; its SHA-256 is the manifest's.
cab_url='https://download.visualstudio.microsoft.com/download/pr/8d9439fc-b60d-44e9-88b6-bae33b111f88/Win11Sdk/Sept2026Servicing26100/WindowsSDK261009457/Installers/61d57a7a82309cd161a854a6f4619e52.cab'
cab_sha256=6ef3e512e5cab49a3f5f0dff57408fb81d29ea591a194f6e5978f87750af21c2
cab_member=fil3585cb2ea5db13cc0838f8d06b5c9679
# d3dcompiler_47.dll 10.0.26100.9457, PE32+ x86-64, "Direct3D HLSL Compiler
# for Redistribution", signed by Microsoft Corporation.
dll_sha256=1da540a0ddbc54de4142570c8ec4a4566ef610fce72c181efb55a9332c480790
dll_version=10.0.26100.9457
# Microsoft Root Certificate Authority 2010, which the signature chains to.
root_url='https://www.microsoft.com/pki/certs/MicRooCerAut_2010-06-23.crt'
root_sha256=df545bf919a2439c36983b54cdfc903dfa4f37d3996d8d84b4c31eec6f3c163e

dry_run=0
while [ $# -gt 0 ]; do
    case "$1" in
        --bottle) [ $# -ge 2 ] || die "--bottle needs a name"; CX_BOTTLE=$2; shift 2 ;;
        --bottle=*) CX_BOTTLE=${1#--bottle=}; shift ;;
        --dry-run) dry_run=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown argument '$1'" ;;
    esac
done

# --- Wine: the same choice as play-macos.sh (duplicated, since that script
# is not written to be sourced) ---------------------------------------------
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
    kind=gptk; wine=$gptk
elif command -v wine64 >/dev/null 2>&1; then
    kind=wine; wine=$(command -v wine64)
elif [ -x "$crossover" ]; then
    kind=crossover; wine=$crossover
else
    die "no Wine found: set WINE, or install CrossOver (/Applications or ~/Applications) or Apple's tap's Wine (docs/macos.md)"
fi
say "wine ($kind): $wine"
wine_cmd=("$wine")
if [ "$kind" = crossover ]; then
    [ -n "${CX_BOTTLE:-}" ] || die "with CrossOver, name the bottle: --bottle B or CX_BOTTLE=B"
    wine_cmd+=(--bottle "$CX_BOTTLE")
    # Asked of CrossOver itself; CX_BOTTLE_PATH (CrossOver's own) otherwise.
    system32=$("${wine_cmd[@]}" winepath -u 'C:\windows\system32' 2>/dev/null | tr -d '\r') || system32=''
    [ -n "$system32" ] \
        || system32="${CX_BOTTLE_PATH:-$HOME/Library/Application Support/CrossOver/Bottles}/$CX_BOTTLE/drive_c/windows/system32"
    system32=${system32%/}
else
    # A bottle is CrossOver's; given to another Wine it would be ignored.
    [ -z "${CX_BOTTLE:-}" ] \
        || die "a bottle ('$CX_BOTTLE') was given, but the Wine chosen is $wine ($kind), not CrossOver: unset WINE/CX_BOTTLE or drop --bottle"
    export WINEPREFIX="${WINEPREFIX:-$HOME/.victory-wine}"
    system32="$WINEPREFIX/drive_c/windows/system32"
fi
[ -d "$system32" ] || die "no $system32: make the bottle or prefix first (docs/macos.md)"
[ -d "$system32/../syswow64" ] \
    || die "no syswow64 beside $system32: the prefix is not 64-bit, and this DLL is the x64 one"
target="$system32/d3dcompiler_47.dll"
backup="$target.builtin.bak"
reg_add=("${wine_cmd[@]}" reg add 'HKCU\Software\Wine\DllOverrides' /v d3dcompiler_47 /d native /f)

sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
installed=0
if [ -f "$target" ] && [ "$(sha256 "$target")" = "$dll_sha256" ]; then installed=1; fi

if [ "$dry_run" = 1 ]; then
    if [ "$installed" = 1 ]; then
        echo "$target is already Microsoft's $dll_version: nothing to download"
    else
        echo "download $cab_url (SHA-256 $cab_sha256)"
        echo "extract $cab_member, check SHA-256 $dll_sha256 and its Microsoft signature"
        if [ -f "$target" ] && [ ! -f "$backup" ]; then printf 'cp %q %q\n' "$target" "$backup"; fi
        printf 'cp <d3dcompiler_47.dll> %q; mv it to %q\n' "$target.tmp" "$target"
    fi
    printf '%q ' "${reg_add[@]}"; echo
    exit 0
fi

curl_get() { curl -fsSL --proto '=https' --retry 3 --connect-timeout 30 --max-time 600 -o "$1" "$2"; }

if [ "$installed" = 1 ]; then
    say "$target is already Microsoft's $dll_version"
else
    command -v osslsigncode >/dev/null 2>&1 \
        || die "osslsigncode is needed to check Microsoft's signature: brew install osslsigncode"
    command -v bsdtar >/dev/null 2>&1 || die "bsdtar is needed to open the cab"

    work=$(mktemp -d "${TMPDIR:-/tmp}/victory-d3dcompiler.XXXXXX")
    trap 'rm -rf "$work"; rm -f "$target.tmp"' EXIT
    mkdir "$work/cab" "$work/out"

    say "downloading Windows SDK $dll_version's cab"
    curl_get "$work/cab/sdk.cab" "$cab_url" || die "download failed: $cab_url"
    got=$(sha256 "$work/cab/sdk.cab")
    [ "$got" = "$cab_sha256" ] || die "cab SHA-256 is $got, expected $cab_sha256"
    bsdtar -xf "$work/cab/sdk.cab" -C "$work/out" "$cab_member" || die "no $cab_member in the cab"
    dll="$work/out/$cab_member"
    got=$(sha256 "$dll")
    [ "$got" = "$dll_sha256" ] || die "d3dcompiler_47.dll SHA-256 is $got, expected $dll_sha256"

    curl_get "$work/root.crt" "$root_url" || die "download failed: $root_url"
    got=$(sha256 "$work/root.crt")
    [ "$got" = "$root_sha256" ] || die "Microsoft root certificate SHA-256 is $got, expected $root_sha256"
    openssl x509 -inform DER -in "$work/root.crt" -out "$work/root.pem" || die "cannot read the root certificate"
    # osslsigncode also fetches the CRLs, over http: it fails offline.
    if ! osslsigncode verify -CAfile "$work/root.pem" -TSA-CAfile "$work/root.pem" -in "$dll" >"$work/verify.txt" 2>&1; then
        cat "$work/verify.txt" >&2
        die "the Authenticode signature does not verify (offline, or a proxy in the way of its CRLs?)"
    fi
    grep -q '^Signature verification: ok' "$work/verify.txt" || die "the Authenticode signature does not verify"
    # The signer's own certificate: the first Subject after "Signer's certificate:".
    signer=$(awk '/^Signer.s certificate:/ { s = 1; next } s && /Subject:/ { sub(/^[ \t]*Subject: */, ""); print; exit }' "$work/verify.txt")
    [ "$signer" = 'CN=Microsoft Corporation,O=Microsoft Corporation,L=Redmond,ST=Washington,C=US' ] \
        || die "the DLL's signer is '$signer', not Microsoft Corporation"
    say "SHA-256 and Microsoft Corporation's signature checked"

    if [ -f "$target" ] && [ ! -f "$backup" ]; then
        cp -p "$target" "$backup"
        say "kept Wine's builtin as $backup"
    fi
    # A new file moved over the old one: never written through a link.
    cp "$dll" "$target.tmp"
    mv -f "$target.tmp" "$target"
    say "installed $target"
fi
"${reg_add[@]}" >/dev/null || die "setting the DLL override failed"
say "DLL override d3dcompiler_47=native set"
