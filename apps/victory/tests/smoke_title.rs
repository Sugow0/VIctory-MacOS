//! End-to-end smoke test: `victory run <exe> --execute` on a synthetic title
//! (tests/fixtures/smoke-title), through the loader, HLE and the GPU layer
//! to the host's swap chain, without any game data. The title clears and
//! presents frames, then returns; the run must say it exited with 0 and
//! the frame meter must have logged frame times.
//!
//! Where it cannot run (no host D3D12 device or no swap chain, as on a
//! machine without a GPU) the end-to-end tests say so and pass; anything
//! else that goes wrong fails them. scripts/smoke-macos.sh runs the same
//! check under Wine on macOS.

/// What a run's output says about it.
#[derive(Debug, PartialEq)]
enum Verdict {
    /// The frame times the meter logged (ms/frame), in order.
    Passed(Vec<f64>),
    /// The run could not show frames here; not a failure of ours.
    Skipped(String),
    Failed(String),
}

/// What the host says when it has no GPU to present with.
const NO_HOST: [&str; 2] = ["no host D3D12 device", "no swap chain"];

/// Judges a run from its exit status and its output (stdout and stderr).
fn verdict(exited_ok: bool, output: &str) -> Verdict {
    let output = strip_ansi(output);
    if !exited_ok {
        return Verdict::Failed(format!(
            "victory did not exit successfully\n{}",
            tail(&output)
        ));
    }
    let Some(code) = output
        .lines()
        .find_map(|l| l.trim().strip_prefix("title exited with "))
    else {
        return Verdict::Failed(format!("no \"title exited with\" line\n{}", tail(&output)));
    };
    if code.trim() != "0" {
        return Verdict::Failed(format!("title exited with {code}\n{}", tail(&output)));
    }
    if let Some(why) = NO_HOST.iter().find(|w| output.contains(*w)) {
        return Verdict::Skipped(format!("{why}: nothing can be presented here"));
    }
    let times = frame_times(&output);
    if times.is_empty() {
        return Verdict::Failed(format!(
            "no \"ms/frame\" line from the frame meter\n{}",
            tail(&output)
        ));
    }
    Verdict::Passed(times)
}

/// The ms/frame of each frame meter line (`"60.0 fps, 16.7 ms/frame (..."`).
fn frame_times(output: &str) -> Vec<f64> {
    output
        .lines()
        .filter_map(|l| {
            let (before, _) = l.split_once(" ms/frame (")?;
            let (_, ms) = before.rsplit_once(" fps, ")?;
            ms.trim().parse().ok()
        })
        .collect()
}

fn median(times: &[f64]) -> f64 {
    let mut sorted = times.to_vec();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // CSI: up to and including the final letter.
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn tail(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    lines[lines.len().saturating_sub(30)..].join("\n")
}

#[test]
fn a_clean_run_passes_with_its_frame_times() {
    let out = "\x1b[2m2026\x1b[0m INFO victory_gpu::host: 60.0 fps, 16.7 ms/frame (0.1 preparing)\n\
               INFO victory_gpu::host: 59.9 fps, 16.8 ms/frame (0.0 preparing)\n\
               title exited with 0\n";
    assert_eq!(verdict(true, out), Verdict::Passed(vec![16.7, 16.8]));
    assert_eq!(median(&[16.8, 8.3, 16.7]), 16.7);
    assert_eq!(median(&[1.0, 3.0]), 2.0);
}

#[test]
fn a_title_that_fails_fails_the_run() {
    let out = "INFO 60.0 fps, 16.7 ms/frame (x)\ntitle exited with 1\n";
    assert!(
        matches!(verdict(true, out), Verdict::Failed(why) if why.starts_with("title exited with 1"))
    );
    // victory itself exits 0 for any title code: the line is what counts.
    let out = "INFO 60.0 fps, 16.7 ms/frame (x)\n";
    assert!(
        matches!(verdict(true, out), Verdict::Failed(why) if why.starts_with("no \"title exited"))
    );
    assert!(matches!(
        verdict(false, "title exited with 0\n"),
        Verdict::Failed(_)
    ));
}

#[test]
fn no_frame_times_fail_the_run_unless_there_is_no_host_gpu() {
    let out = "title exited with 0\n";
    assert!(matches!(verdict(true, out), Verdict::Failed(why) if why.contains("ms/frame")));
    let out = "WARN no host D3D12 device; command lists will not run\ntitle exited with 0\n";
    assert!(matches!(verdict(true, out), Verdict::Skipped(_)));
    // A title failure is a failure even without a GPU.
    let out = "WARN no host D3D12 device; command lists will not run\ntitle exited with 13\n";
    assert!(matches!(verdict(true, out), Verdict::Failed(_)));
}

#[cfg(windows)]
mod end_to_end {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    /// The longest a run may take (the title's frames take 15 s at 60 Hz).
    const TIMEOUT: Duration = Duration::from_secs(180);

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    /// Builds the fixture into its own target dir (the outer cargo holds
    /// the lock on the main one while tests run).
    fn build_title() -> PathBuf {
        let manifest = workspace_root().join("tests/fixtures/smoke-title/Cargo.toml");
        let target = Path::new(env!("CARGO_TARGET_TMPDIR")).join("smoke-title");
        let status = Command::new(env!("CARGO"))
            .args(["build", "--quiet", "--release", "--manifest-path"])
            .arg(&manifest)
            .arg("--target-dir")
            .arg(&target)
            .status()
            .expect("running cargo");
        assert!(status.success(), "building smoke-title failed");
        target.join("release").join("smoke-title.exe")
    }

    /// `victory run <title> --execute -- <args>`, in a folder of its own,
    /// killed after [`TIMEOUT`]: whether it exited successfully, and its
    /// output.
    fn run(args: &[&str]) -> (bool, String) {
        let title = build_title();
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("smoke-title.exe");
        std::fs::copy(&title, &exe).unwrap();
        let out_path = dir.path().join("stdout.txt");
        let err_path = dir.path().join("stderr.txt");
        let mut command = Command::new(env!("CARGO_BIN_EXE_victory"));
        // None of the developer's switches: the run as it is by default.
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("VICTORY_") {
                command.env_remove(key);
            }
        }
        let mut child = command
            .arg("run")
            .arg(&exe)
            .arg("--execute")
            .arg("--")
            .args(args)
            .env("VICTORY_CACHE_DIR", dir.path().join("cache"))
            .env("VICTORY_DATA_DIR", dir.path().join("data"))
            .env("VICTORY_LOG", "info")
            .stdin(Stdio::null())
            .stdout(std::fs::File::create(&out_path).unwrap())
            .stderr(std::fs::File::create(&err_path).unwrap())
            .spawn()
            .expect("starting victory");
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if started.elapsed() > TIMEOUT {
                let _ = child.kill();
                let _ = child.wait();
                let output = std::fs::read_to_string(&err_path).unwrap_or_default();
                panic!(
                    "the run took more than {TIMEOUT:?}\n{}",
                    tail(&strip_ansi(&output))
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let output = std::fs::read_to_string(&out_path).unwrap()
            + &std::fs::read_to_string(&err_path).unwrap();
        (status.success(), output)
    }

    /// 900 frames at the title's 60 Hz: 15 s, well past the meter's first
    /// line (it logs frame times every 5 s from the swap chain's making);
    /// then the same title failing (exit code 3), which must be caught.
    /// One test, so that the two runs (two windows) do not overlap.
    /// `VICTORY_SMOKE_REQUIRE=1` makes a skip a failure.
    #[test]
    fn a_synthetic_title_presents_frames_and_exits_0() {
        let (ok, output) = run(&["900"]);
        match verdict(ok, &output) {
            Verdict::Passed(times) => {
                println!(
                    "smoke: {} meter lines, median {:.1} ms/frame",
                    times.len(),
                    median(&times)
                );
            }
            Verdict::Skipped(why) if std::env::var_os("VICTORY_SMOKE_REQUIRE").is_none() => {
                println!("smoke test skipped: {why}");
            }
            Verdict::Skipped(why) => panic!("skipped, with VICTORY_SMOKE_REQUIRE set: {why}"),
            Verdict::Failed(why) => panic!("{why}"),
        }
        let (ok, output) = run(&["30", "3"]);
        let failing = verdict(ok, &output);
        assert!(
            matches!(failing, Verdict::Failed(ref why) if why.starts_with("title exited with 3")),
            "{failing:?}"
        );
    }
}
