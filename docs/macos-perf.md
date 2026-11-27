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

**Draws** (`--draws N`, title argument 5; section "Draws" below) came later. When this section's figures were taken, nothing in the repo built console shader containers, and a draw without a host pipeline is dropped (`draw.rs`).

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

## Draws

`--draws N` adds N draws of a small triangle to each frame: `DrawInstanced(3, 1, 0, 0)` after one setting of root signature, pipeline, topology, viewport, scissor and target. The shaders are console containers that the fixture's build.rs writes with `victory_shader::native::synth`: hand-encoded GFX10 code in an `XBCH`-packed `FSSM` blob behind an `XDAT` index, the layout titles ship. No game data is used. The vertex program places the triangle from `SV_VertexID` alone, so there is no vertex buffer. The pixel program writes a constant colour. The triangle covers 64x36 pixels (about 1/800 of the 1280x720 target), so fill costs little. The pipeline comes from a stream (device +0x2b0), as titles make theirs. With `--draws`, the script fails a run if any meter line shows 0 draws/frame; draws/frame counts draws actually issued on the host.

Builds and machine are as above, with `victory.exe` release from `port-macos` at 56f6da0 plus this branch. All runs are unpaced. Each configuration ran 3 times in one pass with nothing else using the bottle; the table gives the median of the runs' medians, and the range of those three in parentheses when they differ.

| Draws per frame | `VICTORY_THIN` | ms/frame | preparing ms | on the GPU ms |
|---|---|---|---|---|
| 0 | off | 8.3 | 0.0 | 0.0 |
| 0 | 1 | 8.3 | 0.0 | 0.0 |
| 1,000 | off | 8.3 | 0.0 | 0.0 |
| 1,000 | 1 | 8.3 (8.2-8.3) | 0.0 | 0.0 |
| 10,000 | off | 8.3 (4.3-8.3) | 0.0 | 0.0 |
| 10,000 | 1 | 8.3 | 0.0 | 0.0 |
| 50,000 | off | 8.7 (8.65-8.85) | 0.0 | 0.0 |
| 50,000 | 1 | 8.3 (8.3-8.4) | 0.0 | 0.0 |
| 200,000 | off | 36.9 (35.7-37.1) | 0.0 | 0.0 |
| 200,000 | 1 | 29.8 (28.8-30.4) | 0.0 | 0.0 |
| 500,000 | off | 90.3 (89.1-90.7) | 0.0 | 0.0 |
| 500,000 | 1 | 73.0 (72.2-76.4) | 0.0 | 0.0 |

200,000 and 500,000 draws are there to get off the 120 Hz cap, since 50,000 hardly moves it. One 10,000-draw run went past the cap (4.3 ms/frame), as some runs did before ("Things seen along the way").

Shader translation, 3 runs each with 1,000 draws. "Cold" runs have their own empty VIctory cache, as the script gives every run. "Cached" runs share a cache that one run before them filled (`--env VICTORY_CACHE_DIR=...`). Times are from the log's timestamps.

| | vertex shader | pixel shader | from the first bind to the first present |
|---|---|---|---|
| cold (cache misses 2) | 21.1 ms (20.9-21.5) | 1.5 ms (1.4-1.6) | 70.7 ms (69.5-72.4) |
| cached (cache hits 2) | 4.8 ms (4.6-4.9) | 0.7 ms (0.7) | 56.3 ms (53.5-56.4) |

A shader's time runs from the log's previous line to its "shader translated" line. That is from "host view heaps hold" for the vertex shader, and from the vertex shader's line for the pixel shader. The vertex shader comes first, so its figure probably includes one-time set-up as well as translating it (an assumption: the two programs are about the same size, 322 and 353 bytes). Before that, about 37 ms goes on making the host's view heaps (`probe_view_limit`), cold or cached. After the pixel shader, the host pipeline is made in about 1.8 ms, and the first frame is presented about 10 ms later. D3DMetal's own shader cache lives in the bottle and is shared by every run, so "cold" here is cold for VIctory only.

- **A draw's whole path costs about 0.18 µs, all on the title's recording thread.** From 200,000 to 500,000 draws, the frame grows by 53.4 ms for 300,000 more draws: 0.178 µs a draw. The path is HLE, victory-gpu's state (the bindings are set once a frame here), Wine's D3D12 and D3DMetal's encoding, all x86 code under Rosetta. The meter's "preparing" and "on the GPU" stay at 0.0. They count VIctory's own submission work and its waits for the card, not the title thread's recording, so the frame is the recording. That is about twice a clear (at most 0.1 µs, above), and about 20 times less than upstream's 3.5 µs per call for the real title on Windows. This title's draws change no state between them, though, and real draws do.
- **`VICTORY_THIN=1` takes about 20% off.** It moves translation of the list calls off the title's thread. The slope falls to 0.144 µs a draw (73.0 against 29.8 ms), and 200,000 draws take 29.8 ms instead of 36.9. What remains runs on the title thread or the replaying thread; this measurement does not say which.
- **Translating these two shaders is cheap; what they say about real shaders is little.** The two programs are 10 and 6 instructions long, so their 21 and 1.5 ms (cold) are mostly fixed costs. A warm VIctory cache cuts 22.6 ms to 5.5 ms. The title's 25,790 shaders are much longer.

## Where the time goes

- **A clear's whole path is cheap.** A title call goes through HLE, victory-gpu (straight to the host's `ClearRenderTargetView`, `list.rs`), Wine's D3D12 and D3DMetal, all in x86 code under Rosetta. 100,000 clears a frame cost 9.7 ms in total, so each clear costs at most about 0.1 µs on the recording thread. 10,000 clears or 100 copies don't move off the 120 Hz cap. This only bounds the cheapest call there is. Upstream measured 3.5 µs per list call for the real title on Windows, 45,000-60,000 calls a frame (`crates/victory-gpu/src/list/thin.rs`), because translating a draw's state costs far more than a clear. So this title cannot say what Rosetta does to those calls.
- **VIctory's own preparing is nil here.** With the clears and copies this title has no title memory to copy, no textures to convert and no shaders, so the meter reads 0.0-0.1 ms of preparing throughout.
- **Heavy copies are set by the card, and vary.** `VICTORY_GPU_TIMING` runs each list on its own and times it: it reports 36.2 ms on the GPU out of 38.3 ms per frame for 3,000 copies, so that frame is the card's. Without timing, the meter's "on the GPU" figure (7.2 ms) counts only VIctory's waits for the card, not the card's whole work. The card-clock line shows "nothing of ours queued" at about 0 ms for 3,000 and 10,000 copies. That measures queued time, not how busy the card is. 10,000 copies gave 54 ms a frame in two runs of the clean pass and 124 ms in the other; the first pass gave 79-122 ms. Even the 54 ms runs began with a 264-269 ms window. Counting each copied byte once, the 124 ms runs move about 300 GB/s and 3,000 copies about 310 GB/s; a copy also reads, so the traffic is twice that. The source and destination together are 7.4 MB, which may stay in the GPU's caches, so these figures say nothing reliable about memory bandwidth. Why throughput varies this much is not known: thermal or clock state, or D3DMetal/Metal dropping repeated copies, are guesses.
- **diag costs about 10% CPU and nothing extra on the GPU here.** 100,000 clears take 10.7 ms in diag against 9.7 ms in release, and 3,000 copies are equal in both. `VICTORY_GPU_TIMING` itself adds about 3 ms a frame at 3,000 copies, since it runs lists one at a time.
- **Present and pacing cost nothing visible.** Paced runs hit 16.7 ms exactly and unpaced runs hit the display's 8.3 ms.
- **`sample` of the wine process is of little use.** During a 100,000-clear run the process used about one core (98% CPU in `ps`). The samples are unsymbolised Rosetta frames: 1,927 of about 2,080 top-of-stack samples are at one unknown address.

### Things seen along the way

- **Runs past the 120 Hz cap.** In a pass that overlapped another agent's run in the same bottle, some unpaced runs went past 120 Hz (241 and 484 fps). The likely cause is that presents stop waiting for vsync when the window is hidden (speculation). Those runs then ended before the meter's first 5 s line, and the smoke check failed them ("no ms/frame line"). Unpaced figures are only meaningful with the window visible. At 484 fps the unpaced default of 3,600 frames lasts 7.4 s, so a run that must log meter lines may need more frames.
- **One hang.** One run (3,000 copies, release, first pass) stopped logging after frame 256 and was killed at the 400 s timeout. Its last card-clock line showed 151 ms a frame of the submitting thread working with nothing of ours queued. It did not happen in the 41 runs after it.
- **No shader cache to measure.** The run's shader cache stays empty ("shaders from the cache and translated so far hits=0 misses=0"), so first-run and cached runs can't differ in shader translation here.

## Three levers towards 60 fps on the real title

Upstream's figure is about 45 ms a frame on Windows with an RX 7900 XTX (README: City street, Series S profile, patches 1, 2 and 5-7). There, the one thread that submits the title's work takes about 40 ms and the card about 36 ms. Reaching 60 fps (16.7 ms) on this Mac needs both brought well below that. Neither has been measured here with the game.

1. **Get list-call translation off the title's threads (`VICTORY_THIN=1`) and spread it over the cores.** Measured upstream (`list/thin.rs`): 3.5 µs per call translated inside the call, 160-190 ms of CPU a frame over the title's five recording threads. Measured here: a clear's whole path is at most 0.1 µs; what Wine and D3DMetal cost for a draw's state, pipeline and root bindings is not measured here, as upstream's 3.5 µs is from Windows without Wine. Speculation: under Rosetta that translation runs as translated x86 code and is likely slower than on Windows, and this Mac has 6 performance cores. Deferring the translation to VIctory's threads, as `VICTORY_THIN` does, and with the submitting thread's 40 ms cut by the renderer patches (`VICTORY_PORT_CONSTANTS`, `VICTORY_PORT_CBV`, `VICTORY_PORT_GUARD`, `VICTORY_PORT_JOURNAL`), is the first thing to measure with the game.
2. **Use unified memory instead of copying into "card memory".** Measured: a copy of a 3.7 MB texture with its two barriers costs the card about 12 µs (36.2 ms timed on the GPU for 3,000), and heavy copies set the frame. Speculation: on an Apple GPU, VIctory's host copies of title memory in card memory (`shadow::cards`, twins, uploads) copy bytes that the GPU could read where they are, since CPU and GPU share one memory. Reading title memory in place, as `VICTORY_PORT_CONSTANTS` does for the constants heap, removes both the CPU time and the card time of those copies.
3. **The GPU itself, with a lighter profile and a warm shader cache.** Speculation, not measured: the card's 36 ms on a 7900 XTX is likely several times longer on a 20-core M5 Pro GPU. The Series S profile (lower resolution) is the cheapest lever, and ray tracing must stay off. The title's 25,790 translated shaders also go through D3DMetal's Metal IR converter on first use, so keeping VIctory's and D3DMetal's caches between runs avoids the first-run stutter. The smoke title's two small shaders (section "Draws") cannot check this.

## What could not be measured without a real game copy

- Real draws: the title's own shaders (long GFX10 programs, with resources), their translation time and D3DMetal's conversion to Metal, pipeline creation stalls, and draws that change state between them. The section "Draws" measures a two-shader stand-in only.
- VIctory's preparing on real work: title memory copies, texture conversion, write guards, copying back what shaders wrote, and the real list calls' translation (3.5 µs each on Windows).
- Rosetta's cost compared with native x86. There is no native-Windows run of `victory.exe` on this machine to compare against, and `sample` shows only unsymbolised Rosetta frames.
- The card's time for the game's frame on an Apple GPU, memory pressure (the game takes about 30 GB of private memory), and DirectStorage and file reads through Wine.

## Reproducing

```sh
scripts/build-windows-on-macos.sh --accept-license --locked --release -p victory
scripts/smoke-macos.sh --unpaced --frames 2400 --clears 100000 --timeout 400
scripts/smoke-macos.sh --unpaced --frames 600 --copies 3000 --timeout 400
scripts/smoke-macos.sh --build diag --unpaced --frames 600 --copies 3000 --env VICTORY_GPU_TIMING=1 --timeout 400
scripts/smoke-macos.sh --no-build --unpaced --draws 50000 --env VICTORY_THIN=1
scripts/smoke-macos.sh --no-build --unpaced --frames 900 --draws 500000 --timeout 400
```

The script's first run builds the fixture as well. Each line printed ends with the run's log path. The meter's full lines are in its `run.txt`.
