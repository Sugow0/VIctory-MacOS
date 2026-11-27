# Native macOS: design (draft)

Status: **draft for discussion**, nothing built. Branch `feat/macos-native`,
from `port-macos`. Today's macOS path (Wine/CrossOver + D3DMetal) is
`docs/macos.md`.

## Why

Three reasons, in the user's words: **speed, control, and handing people a
single Mac app**.

- Speed: drop the layers between the title and the GPU that we neither
  measure nor tune (Wine's syscalls and sync, D3DMetal's D3D12 emulation).
- Control: every layer is ours, debuggable and fixable. Today, D3DMetal
  quirks (waves of 64 run wrong, no doubles, `GetAttributeAtVertex` gives 0,
  no pipeline libraries, opaque descriptor handles) can only be worked
  around, never fixed.
- One app: `Victory.app`, opened by double-click. No CrossOver licence, no
  bottle, no GPTK, no `d3dcompiler_47` setup script.

## Goals, non-goals, anti-goals

Goals
- A signed `Victory.app` that runs the title from the user's own files, on
  Apple silicon, without Wine or CrossOver installed.
- Frame time at least as good as the Wine/D3DMetal path once that path is
  measured (it has not run the title yet), then better.
- Keep merging upstream `main` into `port-macos` cheaply.

Non-goals
- Intel Macs.
- Replacing Rosetta 2 before stage 4 (see the Rosetta risk below). Until
  then the title's x86-64 code runs under Rosetta, which is still "native"
  for a Mac app.
- A general Windows compatibility layer: only what this title and
  `victory` need.

Anti-goals (a failure even if it works)
- Game files, code or assets in the repo or the app bundle (asset policy).
- A fork of `victory-gpu` so wide that every upstream merge conflicts.
  Upstream lands several GPU PRs a day.
- Slower than the Wine path, at any stage we ship.

## What ties us to Windows today

Evidence from a read of `feat/macos-native` (file:line, at the branch base).

| Area | Today | Size of the problem |
|---|---|---|
| Host pass-through | The title calls kernel32, kernelbase, ntdll, ucrtbase, vcruntime140*, msvcp140*, ole32, advapi32, ws2_32, bcrypt, crypt32, winhttp, xaudio2_9, mfplat, api-ms-win-* directly (`crates/victory-hle/src/host.rs:17-58`) | **The largest.** About 159 exports are ours. Everything else is real Windows, so native means providing a Win32 user-mode environment |
| Loading | The patched title image is loaded as a DLL by the OS loader (`crates/victory-loader/src/windows.rs:64`), which also does static TLS and unwind tables | We need our own PE mapper: TLS, relocations, `.pdata` |
| Threads | Title `gs:[0x58]` loads are rewritten into thunks (`victory-loader/src/tls.rs`). We read the TEB in `threads.rs:317` and `stacks.rs:143-164` | macOS also uses `%gs` (its TSD), and Rosetta 2 does not keep the GS base in thread state. GS has to be swapped on every PE↔host crossing, as Wine MR 6866 does |
| Exceptions | Write guards, the compiler crash guard and the crash reporter are VEHs on Win32 `CONTEXT` (`victory-gpu/src/watch.rs:3880-3990`, `pipeline/make.rs:201`, `victory-loader/src/crash.rs:113`). Zen 2 instructions are emulated there | Mach exceptions / `SIGSEGV` with a Win64 `CONTEXT` built from them, plus SEH unwinding through title frames |
| Memory | VirtualAlloc2 / MapViewOfFile3 placeholders (64 sites), `MEM_WRITE_WATCH` / `GetWriteWatch`, `NtQueryVirtualMemory` class 7, about 300 plain Virtual* calls, no abstraction | `mach_vm_*` + `mmap` of shared memory; write watching via protection faults. Host pages are 16 KiB, the title assumes 4 KiB |
| GPU | `windows` 0.62 D3D12/DXGI, about 55-60 of 104 files in `victory-gpu` (~3,400 mentions), no backend trait. FXC `D3DCompile` at run time for internal HLSL | Metal underneath. See Approaches |
| Shaders | `victory-shader` emits DXIL, optionally signed with `dxil.dll` | Metal needs metallib: DXIL→Metal IR (Apple's Metal Shader Converter) or our own emitter |
| ABI | 1,065 `extern "system"` exports. On darwin "system" is SysV, but the title calls Win64 | Every HLE export and thunk becomes `extern "win64"` (mechanical) |
| Rest | Window and messages (user32 pass-through), XInput, XAudio2 → WASAPI, Media Foundation, DirectStorage, `\\?\` paths | AppKit window + `CAMetalLayer`, GameController, CoreAudio, AVFoundation, POSIX files |

Every crate already compiles on `aarch64-apple-darwin` in CI, but the
runtime is stubs there (`cfg(windows)`: gpu about 380-430 sites, hle 80, shader 30,
app 29, loader 28, pe 4). `core`, `formats`, `game` and `pipeline` are
already pure.

## Approaches

### A. Ship Wine inside the app

Bundle a Wine build (LGPL) and D3DMetal in `Victory.app`, with a
launcher that makes the prefix itself.

- Gives one app quickly (weeks).
- No speed and almost no control gained: the same layers as today.
- D3DMetal's licence for redistribution is **to verify**. If it cannot be
  shipped, A falls back to DXMT/DXVK+MoltenVK, which are slower or lack
  SM 6.6 / mesh shaders.

### B. Our own Windows layer, all at once

`victory` becomes a Mach-O x86-64 app under Rosetta: our own PE loader,
GS swap, exceptions, memory and every passed-through DLL reimplemented on
macOS, plus a Metal backend.

- Maximum speed and control.
- Many months before anything runs. Nothing is measurable in between,
  and the riskiest part (GS under Rosetta) comes first.

### C. Staged: GPU first, then the Windows layer (chosen)

1. **Our own D3D12 on Metal, under Wine.** Replace D3DMetal with our
   implementation of the D3D12/DXGI interfaces that `victory-gpu` uses,
   written directly on Metal. Keep Wine for everything else.
2. **Single app.** Bundle that Wine plus our D3D12 in `Victory.app`
   (approach A, but with no third-party GPU layer to licence).
3. **Replace Wine piece by piece.** Native loader and GS swap, then
   memory and exceptions, then the passed-through DLLs. Real Microsoft
   ucrt/vcruntime/msvcp DLLs can be mapped by our loader instead of
   rewritten (to verify: licence and what they import). That leaves
   kernel32/ntdll and the system DLLs.

Why C:
- The frame time is set by the GPU path (on Windows, the submit thread is
  about 40 ms and the card about 36 ms). Stage 1 attacks that first, and
  every stage can be measured against the last.
- It fixes the D3DMetal quirks at the source, which is the control asked for.
- **It keeps `victory-gpu` unchanged.** Implementing the D3D12 interfaces
  underneath, rather than adding a backend trait across 60 files, means
  upstream merges stay free. The same D3D12-on-Metal library is reused
  unchanged in stage 3.
- A single app exists from stage 2, long before Wine is gone.

The trade-off: stage 1 is a D3D12 implementation (a subset), which is
large in itself. The subset is bounded by what `victory-gpu` calls, not by
D3D12 as a whole.

## Decided so far

- Branch `feat/macos-native` from `port-macos`; work lands on
  `port-macos` by PR like the rest.
- Apple silicon only. Rosetta 2 for now (see the risk below).
- Goals as above (speed, control, one app).
- **Approach C**, chosen by the user on 2026-11-27.

The user left the next two to "what is better for the final goal
(optimisation, security)". My calls, with the reason:

- **Stage 2's app (Wine inside) is for testers, never a public release.**
  The first public `Victory.app` is the one without Wine. Reasons:
  security (a bundled Wine is a large surface we do not maintain, and it
  needs the hardened runtime's `disable-library-validation`; ours needs
  only `allow-unsigned-executable-memory`, for the title's code and our
  thunks); no users to support on a stack we are removing; and the
  packaging, signing and notarization work of stage 2 is reused unchanged.
- **Minimum macOS 15.4** (AVX2 under Rosetta reported by cpuid from 15.4
  per Apple's forum; see `docs/macos.md`). Developed and measured on
  macOS 27.

## The largest risk: Rosetta 2 ends with macOS 28

Apple said at WWDC 2025 that Rosetta 2 stays fully available through
macOS 27, and from macOS 28 (expected autumn 2027) only a subset is kept
for "older unmaintained games that rely on Intel-based frameworks"
(MacRumors, 2025-06-10; Apple developer forum thread 830692). Whether a
new app that runs x86-64 Windows code falls in that subset is unknown and
unlikely to be promised. This hits every approach, Wine and CrossOver
included, since all of them run the title's x86-64 code through Rosetta.

What it changes:
- Stage 3 must not lean on Rosetta-only tricks (Apple's private Rosetta
  mode for `%gs` used by libd3dshared is out).
- **A stage 4 is needed for the final goal: our own x86-64 → ARM64
  translation of the title's code.** The title is one fixed executable
  that we already patch byte-checked, so translating it ahead of time
  (static recompilation, with a run-time fallback for code it cannot
  find) is plausible, and it would also remove Rosetta's cost and the
  AVX2 question. Prior art to study: FEX-Emu, Box64.
- The order of C stays right: stages 1 and 2 (GPU, packaging) are kept
  whatever replaces Rosetta, and stage 3's loader is the place a
  translator plugs into.

## Stage 1: scope (from the builder check, my calls)

- **Step 0, a baseline.** Run the title once on today's path (CrossOver
  26.3 + D3DMetal, `docs/macos.md`) and record where it gets and its frame
  time. Stage 1 is measured against that. Needs the user's copy of the
  title on this Mac.
- **Step 0b, a Metal Shader Converter spike, before any other stage 1
  work.** Feed it DXIL from our shader cache (wave64, mesh, barycentrics,
  our root signatures) and read its redistribution terms. It is shipped
  inside the app, since shaders are translated while the title runs. If
  it fails, our own DXIL→AIR emitter joins stage 1 (it leaves
  `victory-shader` alone; GCN→AIR would not, and is ruled out for stage 1).
- **Our own Wine, built from upstream Wine** (current stable), not
  CrossOver's prebuilt one or crossover-sources 22.1.1. Our D3D12 is a PE
  `d3d12.dll`/`dxgi.dll` half plus a Mach-O half on Metal, talking
  through Wine's unix-call interface (as D3DMetal and DXMT do), so we
  need a Wine we build against, sign and later bundle (stage 2). Upstream
  also carries the GSBASE swap (MR 6866).
- **The D3D12 subset is the default path only.** Off-by-default port
  switches (title-memory heaps via `OpenExistingHeapFromAddress`,
  `crates/victory-gpu/src/inplace.rs:240-242`), probes, `InfoQueue` and
  DRED answer `E_NOINTERFACE` or fail cleanly for now. Title-memory heaps
  need 16 KiB-aligned `newBufferWithBytesNoCopy` on Metal: later.
- **Done** when the crate tests (`victory-shader`, `victory-gpu`) pass
  against our D3D12 under our Wine, and the title reaches the same point
  as the baseline with a frame time no worse than D3DMetal's.

## Open questions (for the user)

None. The next step is a plan for stage 1.

## To verify before planning (my calls, not decisions)

- Metal Shader Converter: does it accept SM 6.6 DXIL with `WaveSize`,
  mesh/amplification shaders, 64-bit ints, barycentrics and the root
  signature layouts we emit? Can its runtime be redistributed? If no, we
  need our own DXIL→AIR or GCN→AIR emitter.
- How `victory-gpu` reaches D3D12 (the `windows` crate's COM vtables).
  Can our D3D12's Metal half (Mach-O, behind Wine's unix calls in stage 1) be reused as the library in
  stage 3 from the same source?
- FXC: the internal HLSL shaders (`host.rs:1532, 4395`, `inplace.rs:43,
  767`) can be compiled at build time, which removes `d3dcompiler_47`.
- 16 KiB host pages against the title's 4 KiB write-watch granularity
  and 64 KiB views. This matters for stage 3 and already for Wine today.
- Rosetta and GS: Wine MR 6866 swaps GSBASE on PE↔Unix crossings; Apple's
  libd3dshared uses a private Rosetta mode. Which one is open to us?

## Left to the builder

The crate layout of the D3D12-on-Metal library, its test strategy (the
existing `victory-shader` and `victory-gpu` tests run under Wine against
it, as they do against D3DMetal today), and the order of interfaces within
stage 1, guided by what `victory-gpu` calls first at start-up.
