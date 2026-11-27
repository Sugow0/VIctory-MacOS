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

**Real draws are not possible.** A title hands its shaders over as native console GPU code: a DXBC container whose `DXIL` part holds an XDAT index of zlib-packed GFX10 programs (`crates/victory-shader/src/native.rs`). There is no fallback shader. A draw without a host pipeline is dropped (`draw.rs`, counted as "dropped ... without a host pipeline", not in draws/frame). Nothing in the repo can produce a valid GFX10 vertex and pixel shader pair for the fixture; the only builders are test helpers that take hand-encoded instruction words. So the load uses commands that need no shaders. Dispatches and `ExecuteIndirect` would need a compute pipeline and are dropped for the same reason.

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

Start-up: with the bottle warm, the log's first line comes about 0.49 s after launch, the host device is ready at about 0.53 s, and the first frame is presented at about 0.57 s (medians of the 33 clean runs fall between 0.56 and 0.61 s). The first run after a pause took about 1.0-1.2 s to its first present, and some runs during the other pass took up to 1.9 s. In every run, VIctory itself (from its first log line to the first present) takes 0.08-0.18 s. The rest is Wine and Rosetta starting the process.

## Where the time goes

- **The per-command path is cheap.** A title call goes through HLE, victory-gpu, Wine's D3D12 and D3DMetal, all in x86 code under Rosetta. 100,000 clears a frame cost 9.7 ms in total, which puts an upper bound of about 0.1 µs on each clear for the whole path. The process ran at about one core (98% CPU in `sample`). 10,000 clears or 100 copies don't move off the 120 Hz cap. Translated x86 code under Rosetta is not where a frame's time goes, at least not per call.
- **VIctory's own preparing is nil here.** This title has no title memory to copy, no textures to convert and no shaders, so the meter reads 0.0-0.1 ms of preparing throughout. Where the real game spends its time (see below) can't be shown with this title.
- **Copies are GPU-bound and bimodal.** The card's clock line says the card was about 100% busy for 3,000 and 10,000 copies. `VICTORY_GPU_TIMING` runs each list on its own and times it: it reports 36.2 ms on the GPU out of 38.3 ms per frame for 3,000 copies, so the frame is the card's. The meter's "on the GPU" figure without timing (7.2 ms) counts only the waits VIctory makes, not the card's whole work. 10,000 copies ran in one of two stable modes: about 123 ms a frame (the clean pass's first run; the first pass gave 79-122 ms; about 300 GB/s of copies, near the M5 Pro's memory bandwidth) or about 54 ms (later runs, with the GPU meter at 0.0). The 54 ms mode is beyond what the memory could carry for real copies, so D3DMetal or Metal probably drops redundant ones (speculation). Copy throughput on this Mac is therefore only known to within a factor of two.
- **diag costs about 10% CPU and nothing on the GPU here.** 100,000 clears take 10.7 ms in diag against 9.7 ms in release, and 3,000 copies are equal in both. `VICTORY_GPU_TIMING` itself adds about 3 ms a frame at 3,000 copies, since it runs lists one at a time.
- **Present and pacing cost nothing visible.** Paced runs hit 16.7 ms exactly and unpaced runs hit the display's 8.3 ms.

### Things seen along the way

- **Runs past the 120 Hz cap.** In a pass that overlapped another agent's run in the same bottle, some unpaced runs went past 120 Hz (241 and 484 fps). The likely cause is that presents stop waiting for vsync when the window is hidden (speculation). Those runs then ended before the meter's first 5 s line, and the smoke check failed them ("no ms/frame line"). Unpaced figures are only meaningful with the window visible; 2,400 frames is a safer length than the default at light loads.
- **One hang.** One run (3,000 copies, release, first pass) stopped logging after frame 256 and was killed at the 400 s timeout. Its last card-clock line showed 151 ms a frame of the submitting thread working with the card idle. It did not happen again in 32 more runs.
- **No shader cache to measure.** The run's shader cache stays empty ("shaders from the cache and translated so far hits=0 misses=0"), so first-run and cached runs can't differ in shader translation here.

## Three levers towards 60 fps on the real title

Upstream's figure is about 45 ms a frame on Windows with an RX 7900 XTX (README: Series S profile, patches 1, 2 and 5-7). There, the one thread that submits the title's work takes about 40 ms and the card about 36 ms. Reaching 60 fps (16.7 ms) on this Mac needs both brought down well below that. Neither has been measured here with the game.

1. **The submitting thread's own work, not the call path.** Measured: the per-command path costs at most about 0.1 µs (100,000 commands about 10 ms), and preparing is nil when there is nothing to copy or convert. Speculation: the game's 40 ms on that thread is VIctory's preparing (title memory copies, texture conversion, write guards, submission planning). Under Rosetta that thread runs as translated x86 code on one performance core out of 6. The renderer patches that remove that work (`VICTORY_PORT_CONSTANTS`, `VICTORY_PORT_CBV`, `VICTORY_PORT_GUARD`, `VICTORY_PORT_JOURNAL`), and anything that moves the submission's planning onto other cores, should help at least as much here as on Windows.
2. **Use unified memory instead of copying into "card memory".** Measured: bandwidth-bound copies run at about 300 GB/s at best, and the card was about 100% busy with them. Speculation: on an Apple GPU, VIctory's host copies of title memory in card memory (`shadow::cards`, twins, uploads) copy bytes that the GPU could read where they are, since CPU and GPU share one memory. Reading title memory in place, as `VICTORY_PORT_CONSTANTS` does for the constants heap, removes both the CPU time and the bandwidth of those copies.
3. **The GPU itself, with a lighter profile and a warm shader cache.** Speculation, not measured: the card's 36 ms on a 7900 XTX is likely several times longer on a 20-core M5 Pro GPU. The Series S profile (lower resolution) is the cheapest lever, and ray tracing must stay off. The title's 25,790 translated shaders also go through D3DMetal's Metal IR converter on first use, so keeping VIctory's and D3DMetal's caches between runs avoids the first-run stutter. This title has no shaders, so none of this could be checked here.

## What could not be measured without a real game copy

- Real draws: title shaders (GFX10), their translation time to DXIL and D3DMetal's conversion to Metal, first-run vs cached shader times, and pipeline creation stalls.
- VIctory's preparing on real work: title memory copies, texture conversion, write guards, copying back what shaders wrote. These are the 40 ms of the submitting thread on Windows.
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
