# Running on macOS (Apple silicon)

victory.exe stays a Windows program. On a Mac it runs under Wine, with
Direct3D 12 translated to Metal by Apple's D3DMetal, which ships in the Game
Porting Toolkit and in CrossOver. `scripts/play-macos.sh` is the counterpart
of `scripts/play.ps1`.

## Status: untested with the title

Nothing below has been seen running the title yet. The launcher's options,
checks and the command it builds are checked (`--dry-run`); what Wine and
D3DMetal do with victory.exe is not. Known risks:

- **Heaps opened on title memory.** `OpenExistingHeapFromAddress(1)`
  (`crates/victory-gpu/src/inplace.rs:240`, `open`) is what renderer port 1
  (`VICTORY_PORT_CONSTANTS=1`, off by default, `port.rs:207`) and the
  in-place census use. D3DMetal may not implement it, or only on memory it
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
- **AVX2 under Rosetta.** victory.exe is built for AVX2; Rosetta translates
  it from macOS 15 on, at a cost not measured here.

Try `--safe` first.

## What you need

- A Mac with Apple silicon, macOS 15 (Sequoia) or later, Rosetta 2
  (`softwareupdate --install-rosetta --agree-to-license`).
- 64 GB of unified memory recommended, as on Windows (README.md): the title
  takes about 30 GB of private memory plus about 10.5 GB on the GPU side.
- A Wine with D3DMetal: Apple's Game Porting Toolkit 2 or later (its
  `wine64`), or CrossOver 24 or later.
- Rust (the toolchain is pinned) and your own dump of the title.

## Building

- Native tools, the platform-independent parts (being enabled in issue #2):

  ```sh
  cargo build --locked --target aarch64-apple-darwin
  ```

- victory.exe itself, cross-built on the Mac (issue #3):

  ```sh
  scripts/build-windows-on-macos.sh
  ```

  It lands in `target/x86_64-pc-windows-msvc/<profile>/victory.exe`. A
  victory.exe built on Windows and copied to `target/<profile>/victory.exe`
  is used when the first one is missing.

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
| `-RayTracing` | `--ray-tracing` (also `D3DM_SUPPORT_DXR=1`) |
| `-Build diag\|release` | `--build diag\|release` |
| `-More @{K='V'}` | `--more K=V`, repeatable |
| | `--dry-run`: prints the environment and the command |

Which Wine: `WINE` if set; else `wine64` on `PATH` or in
`/usr/local/opt/game-porting-toolkit/bin` (Game Porting Toolkit, with
`WINEESYNC=1` as Apple's read-me has it); else CrossOver's
`/Applications/CrossOver.app/Contents/SharedSupport/CrossOver/bin/wine`,
in the bottle `CX_BOTTLE` names. Outside CrossOver the prefix is
`WINEPREFIX`, by default `~/.victory-wine`, made with `wineboot --init` the
first time.

The ahead-of-time steps (README.md) run the same way, through Wine:

```sh
wine64 target/x86_64-pc-windows-msvc/diag/victory.exe shaders <the shader archives>
wine64 target/x86_64-pc-windows-msvc/diag/victory.exe warm --used
```

To pick an adapter, as on Windows: `--more VICTORY_D3D12_ADAPTER=<index or name>`.
