# Where the time goes on a Mac (Wine, Rosetta, D3DMetal)

These measurements use the synthetic smoke title (`tests/fixtures/smoke-title`) with a load added on top. No game data was used. They show what the layers between a title and the Mac's GPU cost: Wine, Rosetta, D3DMetal, and VIctory's HLE and GPU layer. They are not a measurement of the real game. The section "What could not be measured" lists what only a real copy of the game would show.

## Setup

| | |
|---|---|
| Machine | Mac17,8, Apple M5 Pro (6 performance + 12 efficiency cores, 20-core GPU), 64 GB (`sysctl -n machdep.cpu.brand_string hw.memsize`: `Apple M5 Pro`, `68719476736`) |
| Display | built-in 3456x2234 ProMotion (presents cap at 120 Hz), plus a 1080p external |
| macOS | 27.2.1 (26B131) |
| CrossOver | 26.3 (Wine 11.0, D3DMetal 3.0), bottle `victory` |
| Builds | `victory.exe` release and diag (`--features diagnostics`), cross-built with `scripts/build-windows-on-macos.sh` from `port-macos` at aaa63ce plus this branch |

Each run is one `scripts/smoke-macos.sh --no-build` call, which gives each run its own folder, cache and data. Runs are unpaced (`--unpaced`: no frame clock) unless the table says otherwise. Each configuration ran 3 times in one pass with nothing else using the bottle. The table gives the median across those runs; each run's own figure is the median of its frame-meter lines, logged every ~5 s. Start-up times are wall-clock times from the script's start to the log's first timestamp, to `host D3D12 device ready`, and to the first `frame presented`.

### The load

The fixture can now add a per-frame load (`--clears N`, `--copies N` on the script; title arguments 3 and 4):

- **clears:** N more `ClearRenderTargetView` calls on the 1280x720 RGBA8 target.
- **copies:** N `CopyResource` calls of the target into a second 1280x720 texture (3.7 MB each). Each copy sits between two transition barriers (render target to copy source, and back).

**Real draws are not built here.** A title hands its shaders over as native console GPU code: a DXBC container whose `DXIL` part holds an XDAT index of GFX10 programs, zlib-packed or as plain FSSM blobs (`crates/victory-shader/src/native.rs`). There is no fallback shader, and a draw without a host pipeline is dropped (`draw.rs`, counted as "dropped ... without a host pipeline", not in draws/frame). Nothing in the repo builds such a container for a title. The container layout is documented in `native.rs`, though, and the translator's tests hand-encode small GFX10 pixel programs that translate, so a minimal vertex and pixel shader pair could be made with that work. Until then the load uses commands that need no shaders. Dispatches and `ExecuteIndirect` would need a compute pipeline and are dropped for the same reason.

## Results

Medians of 3 runs each (one clean pass). "preparing" and "on the GPU" are the frame meter's split (`crates/victory-gpu/src/host.rs`, `fn frame`).

| Load per frame | Build | fps | ms/frame | preparing ms | on the GPU ms | first present |
|---|---|---|---|---|---|---|
| none, paced (the default run) | release | 60.0 | 16.7 | 0.0 | 0.0 | 0.57 s |
| none | release | 120.0 | 8.3 | 0.0 | 0.0 | 0.59 s |
| 10,000 clears | release | 120.0 | 8.3 | 0.0 | 0.0 | 0.56 s |
| 100,000 clears | release | 103.6 | 9.7 | 0.0 | 0.0 | 0.57 s |
| 100,000 clears | diag | 93.6 | 10.7 | 0.0 | 0.0 | 0.57 s |
| 100 copies (+200 barriers) | release | 120.0 | 8.3 | 0.0 | 0.0 | 0.57 s |
| 1,000 copies | release | 97.0 | 10.3 | 0.0 | 0.6 | 0.57 s |
| 3,000 copies | release | 28.4 | 35.2 (31.6-35.2) | 0.0 | 7.2 | 0.59 s |
| 3,000 copies | diag | 28.4 | 35.2 (30.2-35.2) | 0.0 | 7.2 | 0.57 s |
| 3,000 copies, `VICTORY_GPU_TIMING=1` | diag | 26.2 | 38.3 (36.5-42.6) | 0.0 | 36.2 | 0.61 s |
| 10,000 copies | release | 18.4 | 54.3 (54.3-123.7) | 0.0 | 0.0 | 0.56 s |

Start-up: with the bottle warm, the log's first line comes about 0.49 s after launch, the host device is ready at about 0.53 s, and the first frame is presented at about 0.57 s. These are medians of the 33 clean runs; each configuration's own median falls between 0.56 and 0.61 s. Some runs, often the first after a pause, took 0.85-1.26 s to their first present, and one took 2.1 s. In every run, VIctory itself (from its first log line to the first present) takes 0.07-0.18 s. The rest is Wine and Rosetta starting the process, plus the script's own start (launch time is taken before `smoke-macos.sh` runs).

## Where the time goes

- **A clear's whole path is cheap.** A title call goes through HLE, victory-gpu (straight to the host's `ClearRenderTargetView`, `list.rs`), Wine's D3D12 and D3DMetal, all in x86 code under Rosetta. 100,000 clears a frame cost 9.7 ms in total, so each clear costs at most about 0.1 µs on the recording thread. 10,000 clears or 100 copies don't move off the 120 Hz cap. This only bounds the cheapest call there is. Upstream measured 3.5 µs per list call for the real title on Windows, 45,000-60,000 calls a frame (`crates/victory-gpu/src/list/thin.rs`), because translating a draw's state costs far more than a clear. So this title cannot say what Rosetta does to those calls.
- **VIctory's own preparing is nil here.** This title has no title memory to copy, no textures to convert and no shaders, so the meter reads 0.0-0.1 ms of preparing throughout.
- **Heavy copies are set by the card, and vary.** `VICTORY_GPU_TIMING` runs each list on its own and times it: it reports 36.2 ms on the GPU out of 38.3 ms per frame for 3,000 copies, so that frame is the card's. Without timing, the meter's "on the GPU" figure (7.2 ms) counts only VIctory's waits for the card, not the card's whole work. The card-clock line shows "nothing of ours queued" at about 0 ms for 3,000 and 10,000 copies. That measures queued time, not how busy the card is. 10,000 copies gave 54 ms a frame in two runs of the clean pass and 124 ms in the other; the first pass gave 79-122 ms. Even the 54 ms runs began with a 264-269 ms window. Counting each copied byte once, the 124 ms runs move about 300 GB/s and 3,000 copies about 310 GB/s; a copy also reads, so the traffic is twice that. The source and destination together are 7.4 MB, which may stay in the GPU's caches, so these figures say nothing reliable about memory bandwidth. Why throughput varies this much is not known: thermal or clock state, or D3DMetal/Metal dropping repeated copies, are guesses.
- **diag costs about 10% CPU and nothing extra on the GPU here.** 100,000 clears take 10.7 ms in diag against 9.7 ms in release, and 3,000 copies are equal in both. `VICTORY_GPU_TIMING` itself adds about 3 ms a frame at 3,000 copies, since it runs lists one at a time.
- **Present and pacing cost nothing visible.** Paced runs hit 16.7 ms exactly and unpaced runs hit the display's 8.3 ms.
- **`sample` of the wine process is of little use.** During a 100,000-clear run the process used about one core (98% CPU in `ps`). The samples are unsymbolised Rosetta frames, and most of them sit in one waiting address.

### Things seen along the way

- **Runs past the 120 Hz cap.** In a pass that overlapped another agent's run in the same bottle, some unpaced runs went past 120 Hz (241 and 484 fps). The likely cause is that presents stop waiting for vsync when the window is hidden (speculation). Those runs then ended before the meter's first 5 s line, and the smoke check failed them ("no ms/frame line"). Unpaced figures are only meaningful with the window visible. At those rates even the unpaced default of 3,600 frames lasts under 10 s, so a run that must log meter lines needs more frames.
- **One hang.** One run (3,000 copies, release, first pass) stopped logging after frame 256 and was killed at the 400 s timeout. Its last card-clock line showed 151 ms a frame of the submitting thread working with nothing of ours queued. It did not happen in the 41 runs after it.
- **No shader cache to measure.** The run's shader cache stays empty ("shaders from the cache and translated so far hits=0 misses=0"), so first-run and cached runs can't differ in shader translation here.

## Three levers towards 60 fps on the real title

Upstream's figure is about 45 ms a frame on Windows with an RX 7900 XTX (README: City street, Series S profile, patches 1, 2 and 5-7). There, the one thread that submits the title's work takes about 40 ms and the card about 36 ms. Reaching 60 fps (16.7 ms) on this Mac needs both brought well below that. Neither has been measured here with the game.

1. **Get list-call translation off the title's threads (`VICTORY_THIN=1`) and spread it over the cores.** Measured upstream (`list/thin.rs`): 3.5 µs per call translated inside the call, 160-190 ms of CPU a frame over the title's five recording threads. Measured here: the bare call path (a clear) is at most 0.1 µs, so the cost is in VIctory's translation, not in Wine or D3DMetal. Speculation: under Rosetta that translation runs as translated x86 code and is likely slower than on Windows, and this Mac has 6 performance cores. Deferring the translation to VIctory's threads, as `VICTORY_THIN` does, and with the submitting thread's 40 ms cut by the renderer patches (`VICTORY_PORT_CONSTANTS`, `VICTORY_PORT_CBV`, `VICTORY_PORT_GUARD`, `VICTORY_PORT_JOURNAL`), is the first thing to measure with the game.
2. **Use unified memory instead of copying into "card memory".** Measured: a copy of a 3.7 MB texture costs the card about 5-12 µs (12 µs at 3,000 a frame), and heavy copies set the frame. Speculation: on an Apple GPU, VIctory's host copies of title memory in card memory (`shadow::cards`, twins, uploads) copy bytes that the GPU could read where they are, since CPU and GPU share one memory. Reading title memory in place, as `VICTORY_PORT_CONSTANTS` does for the constants heap, removes both the CPU time and the card time of those copies.
3. **The GPU itself, with a lighter profile and a warm shader cache.** Speculation, not measured: the card's 36 ms on a 7900 XTX is likely several times longer on a 20-core M5 Pro GPU. The Series S profile (lower resolution) is the cheapest lever, and ray tracing must stay off. The title's 25,790 translated shaders also go through D3DMetal's Metal IR converter on first use, so keeping VIctory's and D3DMetal's caches between runs avoids the first-run stutter. This title has no shaders, so none of this could be checked here.

## What could not be measured without a real game copy

- Real draws: title shaders (GFX10), their translation time to DXIL and D3DMetal's conversion to Metal, first-run vs cached shader times, and pipeline creation stalls.
- VIctory's preparing on real work: title memory copies, texture conversion, write guards, copying back what shaders wrote, and the real list calls' translation (3.5 µs each on Windows).
- Rosetta's cost compared with native x86. There is no native-Windows run of `victory.exe` on this machine to compare against, and `sample` shows only unsymbolised Rosetta frames.
- The card's time for the game's frame on an Apple GPU, memory pressure (the game takes about 30 GB of private memory), and DirectStorage and file reads through Wine.

## Reproducing

```sh
scripts/build-windows-on-macos.sh --accept-license --locked --release -p victory
scripts/smoke-macos.sh --unpaced --frames 2400 --clears 100000 --timeout 400
scripts/smoke-macos.sh --unpaced --frames 600 --copies 3000 --timeout 400
scripts/smoke-macos.sh --build diag --unpaced --frames 600 --copies 3000 --env VICTORY_GPU_TIMING=1 --timeout 400
```

The script's first run builds the fixture as well. Each line printed ends with the run's log path. The meter's full lines are in its `run.txt`.
