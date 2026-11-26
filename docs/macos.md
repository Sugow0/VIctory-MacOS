# Running on macOS (Apple silicon)

victory.exe stays a Windows program. On a Mac it runs under Wine, with
Direct3D 12 translated to Metal by Apple's D3DMetal. Apple's Game Porting
Toolkit 2 and later ship only D3DMetal, no Wine: the Wine is either one built
from Apple's Homebrew tap (formula `game-porting-toolkit`, still 1.1, Wine
from crossover-sources 22.1.1) with GPTK 2+'s D3DMetal libraries copied in,
or CrossOver, which bundles D3DMetal. `scripts/play-macos.sh` is the counterpart
of `scripts/play.ps1`.

## Status: untested with the title

Nothing below has been seen running the title yet. The launcher's options,
checks and the command it builds are checked (`--dry-run`); what Wine and
D3DMetal do with victory.exe is not. Known risks:

- **Heaps opened on title memory.** `OpenExistingHeapFromAddress(1)`
  (`crates/victory-gpu/src/inplace.rs:240`, `open`) is what renderer port 1
  (`VICTORY_PORT_CONSTANTS=1`, off by default; read at
  `crates/victory-hle/src/modules/xmem.rs:1088` and
  `crates/victory-gpu/src/port.rs:78`, the heap opened at `port.rs:207`) and
  the in-place census use. D3DMetal may not implement it, or only on memory it
  allocated itself; then those paths fail and the copies stay.
- **Mesh shaders** (`DispatchMesh`, `crates/victory-gpu/src/draw.rs:3281`):
  D3DMetal maps them onto Metal's mesh shaders; which of the title's
  translated pipelines it accepts is unknown.
- **Placeholders and write watching.** The title's physical pages are
  sections shown through placeholders (`MEM_RESERVE_PLACEHOLDER`,
  `MapViewOfFile3`) and title GPU memory is reserved with write watching
  (`MEM_WRITE_WATCH`, `GetWriteWatch`), both in
  `crates/victory-hle/src/modules/xmem.rs`, read by
  `crates/victory-gpu/src/watch.rs`.
  The runtime already detects Wine (`on_wine`, from ntdll's
  `wine_get_version`) and commits sections whole there; whether Wine on
  macOS (16 KiB host pages) implements placeholders and write watching
  with 4 KiB dirty bits and 64 KiB views is unknown.
- **FXC.** A few host shaders are compiled at run time with `D3DCompile`
  (`inplace.rs`, `host.rs`), which needs `d3dcompiler_47.dll` in the prefix.
  Wine's builtin (vkd3d) refuses 8 of them, so Microsoft's is put in the
  prefix (see "Microsoft's d3dcompiler_47" below).
- **AVX2 under Rosetta.** victory.exe is built for AVX2 and checks it by
  cpuid. The sources disagree, and none of this is tested here:
  AppleGamingWiki has Rosetta's AVX2 from macOS 15.0; Apple's developer
  forums (<https://developer.apple.com/forums/thread/769486>) from 15.4, and
  reported by cpuid only with `ROSETTA_ADVERTISE_AVX=1`. The launcher sets
  `ROSETTA_ADVERTISE_AVX=1`, refuses macOS below 15.0 and warns below 15.4.
  Its cost is not measured.
- **Ray tracing** under D3DMetal is untested; `--ray-tracing` only sets what
  `-RayTracing` sets on Windows.

Try `--safe` first.

## What you need

- A Mac with Apple silicon, macOS 15 (Sequoia) or later, 15.4 or later
  preferred (see AVX2 above), Rosetta 2
  (`softwareupdate --install-rosetta --agree-to-license`).
- 64 GB of unified memory recommended, as on Windows (README.md): the title
  takes about 30 GB of private memory plus about 10.5 GB on the GPU side.
- A Wine with D3DMetal: a Wine built from Apple's tap, with the D3DMetal
  libraries of GPTK 2 or later copied in, or CrossOver 24 or later.
- Rust (the toolchain is pinned) and your own dump of the title.

## Building

- Native tools, the platform-independent parts (being enabled in issue #2):

  ```sh
  cargo build --locked --target aarch64-apple-darwin
  ```

- victory.exe itself, cross-built on the Mac (issue #3):

  ```sh
  brew install llvm lld
  ~/.cargo/bin/cargo install --locked cargo-xwin
  scripts/build-windows-on-macos.sh --accept-license
  ```

  It uses rustup's cargo, Homebrew's `clang-cl` and `lld-link`, and the
  Microsoft CRT and Windows SDK that cargo-xwin downloads on first use
  (about 1 GB in `~/Library/Caches/cargo-xwin`); `--accept-license` (or
  `XWIN_ACCEPT_LICENSE=1`) accepts their license. Arguments replace the
  default `--locked --profile diag -p victory --features diagnostics`, for
  example `scripts/build-windows-on-macos.sh --accept-license --locked
  --release -p victory`; the target is always `x86_64-pc-windows-msvc`. It
  lands in `target/x86_64-pc-windows-msvc/<profile>/victory.exe` and the
  script prints the path. A victory.exe built on Windows (README.md) and
  copied to `target/diag/victory.exe` here also works: the launcher uses it
  when the first one is missing.

## Microsoft's d3dcompiler_47

Wine's builtin `d3dcompiler_47` (vkd3d) rejects `InterlockedAdd`/`InterlockedOr`
on `RWByteAddressBuffer`, `GetDimensions` on `RWTexture2D`/`RWTexture3D` and
`Load4` in the runtime's internal HLSL shaders (indirect, ambient,
lightprobe; issue #16). Once per prefix or bottle:

```sh
brew install osslsigncode
scripts/macos-setup-bottle.sh --bottle <bottle>   # CrossOver
scripts/macos-setup-bottle.sh                     # Wine/GPTK: WINEPREFIX, by default ~/.victory-wine
```

It picks Wine as the launcher does, refuses a bottle when the Wine picked is
not CrossOver, and asks CrossOver where the bottle is (`winepath`; else
`CX_BOTTLE_PATH`, by default `~/Library/Application Support/CrossOver/Bottles`).
It needs a 64-bit prefix (`syswow64` present), downloads nothing when the
DLL in place is already the one below, downloads to a temporary folder
otherwise, and stops on any mismatch. `--dry-run` prints what it would do.
`osslsigncode` fetches the certificates' revocation lists (CRLs) over
http, so the signature check fails offline or behind a proxy that blocks
them.

- Source: the Windows SDK 10.0.26100.9457 (September 2026 servicing), as
  Visual Studio's installer manifest lists it (package
  `Win11SDK_10.0.26100`, msi "Windows SDK for Windows Store Apps
  Tools-x86_en-us.msi"), from Microsoft's
  `https://download.visualstudio.microsoft.com/download/pr/8d9439fc-b60d-44e9-88b6-bae33b111f88/Win11Sdk/Sept2026Servicing26100/WindowsSDK261009457/Installers/61d57a7a82309cd161a854a6f4619e52.cab`,
  SHA-256 `6ef3e512e5cab49a3f5f0dff57408fb81d29ea591a194f6e5978f87750af21c2`
  (the manifest's).
- The DLL, member `fil3585cb2ea5db13cc0838f8d06b5c9679` of that cab:
  `d3dcompiler_47.dll` 10.0.26100.9457, "Direct3D HLSL Compiler for
  Redistribution", PE32+ x86-64, SHA-256
  `1da540a0ddbc54de4142570c8ec4a4566ef610fce72c181efb55a9332c480790`.
  Its Authenticode signature (signer Microsoft Corporation, issued by
  Microsoft Windows Code Signing PCA 2024) is checked with `osslsigncode`
  up to Microsoft Root Certificate Authority 2010, fetched from
  `https://www.microsoft.com/pki/certs/MicRooCerAut_2010-06-23.crt` and
  pinned by its SHA-256
  (`df545bf919a2439c36983b54cdfc903dfa4f37d3996d8d84b4c31eec6f3c163e`).
- It goes to `drive_c/windows/system32/d3dcompiler_47.dll` (Wine's builtin
  kept once as `d3dcompiler_47.dll.builtin.bak`), with the override
  `HKCU\Software\Wine\DllOverrides` `d3dcompiler_47` = `native`.
- The DLL is Microsoft's, under the Windows SDK's license terms; it is not in
  this repository and is not redistributed by it.

To undo (CrossOver, `wine` being
`CrossOver.app/Contents/SharedSupport/CrossOver/bin/wine`; elsewhere
`WINEPREFIX=<prefix> wine64` in place of `wine --bottle <bottle>`, and the
prefix's own `drive_c/windows/system32`):

```sh
cd "$HOME/Library/Application Support/CrossOver/Bottles/<bottle>/drive_c/windows/system32"
if [ -f d3dcompiler_47.dll.builtin.bak ]; then
    mv d3dcompiler_47.dll.builtin.bak d3dcompiler_47.dll
else
    rm d3dcompiler_47.dll   # there was no builtin file to keep; Wine uses its own
fi
wine --bottle <bottle> reg delete 'HKCU\Software\Wine\DllOverrides' /v d3dcompiler_47 /f
```

## Running

```sh
cp victory.example.toml victory.toml   # then set your paths, outside this folder
scripts/play-macos.sh --exe <path of your copy's executable> --safe
```

The paths in `victory.toml` are read by a Windows program: write them as
Wine sees them, with the `Z:` drive for the Unix root
(`game_dir = 'Z:\Users\me\victory-data\game'`). The `VICTORY_CONFIG`,
`VICTORY_GAME_DIR`, `VICTORY_CACHE_DIR` and `VICTORY_DATA_DIR` variables may
be given as Unix paths: the launcher turns them into `Z:` paths.

| play.ps1 | play-macos.sh |
|---|---|
| `-Exe <path>` | `--exe <path>` |
| `-Safe` | `--safe` |
| `-RayTracing` | `--ray-tracing` |
| `-Build diag\|release` | `--build diag\|release` |
| `-More @{K='V'}` | `--more K=V`, repeatable |
| | `--dry-run`: prints the environment and the command |

Which Wine (the launcher always prints the one it chose): `WINE` if set;
else `/usr/local/opt/game-porting-toolkit/bin/wine64` (Apple's tap, with
`WINEESYNC=1`); else a `wine64` on `PATH`, with a warning, since whether it
has D3DMetal is not checked; else CrossOver's
`CrossOver.app/Contents/SharedSupport/CrossOver/bin/wine`, looked for in
`/Applications` first, then in `~/Applications`.
Outside CrossOver the prefix is `WINEPREFIX`, by default `~/.victory-wine`,
made with `wineboot --init` the first time.

With CrossOver, `CX_BOTTLE` is required: make a bottle (Windows 10 or 11, 64-bit)
and turn D3DMetal on in its settings. CrossOver would otherwise use its
default bottle, which may not have D3DMetal on, so the launcher refuses to
guess.

Both steps can be done from the command line. `<crossover>` below is
`/Applications/CrossOver.app/Contents/SharedSupport/CrossOver` or
`~/Applications/CrossOver.app/Contents/SharedSupport/CrossOver`, wherever
CrossOver is installed. Make the bottle:

```sh
<crossover>/bin/cxbottle --bottle victory --create --template win11_64
```

Then turn D3DMetal on by adding, under `[EnvironmentVariables]` in
`~/Library/Application Support/CrossOver/Bottles/victory/cxbottle.conf`:

```ini
"CX_GRAPHICS_BACKEND" = "d3dmetal"
```

Edit it with CrossOver closed: choosing D3DMetal in the bottle's settings
writes the same line, and the settings can rewrite it.

CrossOver 26.3's bottle wrapper accepts `d3dmetal`, `dxmt`, `dxvk` and
`wined3d` there. Then run with `CX_BOTTLE=victory`.

The ahead-of-time steps (README.md) run the same way, through Wine:

```sh
wine64 target/x86_64-pc-windows-msvc/diag/victory.exe shaders <the shader archives>
wine64 target/x86_64-pc-windows-msvc/diag/victory.exe warm --used
```

To pick an adapter, as on Windows: `--more VICTORY_D3D12_ADAPTER=<index or name>`.
