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
  Wine has its own builtin; if it refuses those shaders, a native
  `d3dcompiler_47.dll` is needed.
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
