//! Dev builds only: a memory self-test for CI. `KOMPACTER_SELFTEST="a.mp4;b.wav;dir;c.zip"`
//! makes the app compress each file (twice), hiding and reopening the panel between jobs,
//! and measure its own memory along the way, the way a user would notice it in a task
//! manager. It prints one line per measurement and quits with code 1 if memory keeps
//! growing from one round to the next or stays high once the panel is hidden, 2 if a job
//! fails. Runs the same on Windows, macOS and Linux, with no screen automation.
//!
//! After the jobs it stays hidden for 20 s and checks it used (almost) no CPU: nothing
//! should run while the panel is hidden.
//!
//! `KOMPACTER_SELFTEST_MB` sets the target size (default 5), `KOMPACTER_SELFTEST_SLACK`
//! how much the hidden app may grow over its first hidden reading, in MB (default 40).

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use kompacter_core::Mode;
use slint::{Timer, TimerMode};

use super::App;
use crate::frontend::View;

/// Each file is compressed this many times, so a leak shows as growth between rounds.
const ROUNDS: usize = 2;
/// Growth allowed between the first and the last hidden reading of the same file.
const ROUND_SLACK_MB: f64 = 15.0;
const JOB_TIMEOUT: Duration = Duration::from_secs(600);
/// How long the hidden app is watched for CPU use at the end, and how much it may use
/// (share of one core).
const IDLE_WATCH: Duration = Duration::from_secs(20);
const IDLE_CPU_LIMIT: f64 = 0.02;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Step {
    /// Panel shown at start; settle, then hide for the baseline.
    Start,
    Baseline,
    Open,
    WaitBig,
    WaitDone,
    Hide,
    Hidden,
    /// All jobs done: hidden, measuring CPU use.
    Idle,
}

struct Test {
    files: Vec<PathBuf>,
    /// Index into files × rounds.
    job: usize,
    step: Step,
    since: Instant,
    baseline: f64,
    /// Hidden readings per file, in rounds.
    hidden: Vec<Vec<f64>>,
    peak: f64,
    failed: Option<String>,
    /// CPU seconds used when the idle watch started.
    idle_cpu: f64,
    /// Set when the hidden app used too much CPU.
    failed_idle: Option<String>,
}

pub fn start(app: &Rc<App>, list: &str) {
    let files: Vec<PathBuf> = list
        .split(';')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();
    let mb = std::env::var("KOMPACTER_SELFTEST_MB")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(5);
    {
        let mut s = app.settings.borrow_mut();
        s.preset = "custom".into();
        s.custom_bytes = mb * 1024 * 1024;
        s.auto_hide = false;
    }
    let test = Rc::new(RefCell::new(Test {
        hidden: vec![Vec::new(); files.len()],
        files,
        job: 0,
        step: Step::Start,
        since: Instant::now(),
        baseline: 0.0,
        peak: 0.0,
        failed: None,
        idle_cpu: 0.0,
        failed_idle: None,
    }));
    println!(
        "selftest: {} file(s), target {mb} MB",
        test.borrow().files.len()
    );
    app.show_with(false);
    let timer = Rc::new(Timer::default());
    let weak = Rc::downgrade(app);
    let keep = timer.clone();
    let busy = Rc::new(Cell::new(false));
    timer.start(TimerMode::Repeated, Duration::from_millis(250), move || {
        let _keep = &keep;
        let Some(app) = weak.upgrade() else { return };
        if busy.replace(true) {
            return;
        }
        tick(&app, &test);
        busy.set(false);
    });
}

fn tick(app: &Rc<App>, test: &Rc<RefCell<Test>>) {
    let mut t = test.borrow_mut();
    let waited = t.since.elapsed();
    let mem = memory_mb();
    t.peak = t.peak.max(mem);
    let next = |t: &mut Test, step: Step| {
        t.step = step;
        t.since = Instant::now();
    };
    match t.step {
        Step::Start if waited > Duration::from_secs(2) => {
            println!("selftest: shown at start: {mem:.1} MB");
            app.close();
            next(&mut t, Step::Baseline);
        }
        Step::Baseline if waited > Duration::from_secs(3) => {
            t.baseline = mem;
            println!("selftest: hidden at start (baseline): {mem:.1} MB");
            next(&mut t, Step::Open);
        }
        Step::Open => {
            if t.job >= t.files.len() * ROUNDS {
                t.idle_cpu = cpu_seconds();
                println!(
                    "selftest: all jobs done; watching the hidden app's CPU for {}s",
                    IDLE_WATCH.as_secs()
                );
                next(&mut t, Step::Idle);
                return;
            }
            let file = t.files[t.job % t.files.len()].clone();
            println!("selftest: job {} · {}", t.job + 1, file.display());
            app.show_with(false);
            next(&mut t, Step::WaitBig);
            drop(t);
            app.open(file);
        }
        Step::WaitBig => match app.view() {
            View::Big => {
                next(&mut t, Step::WaitDone);
                let batch = !app.batch.borrow().is_empty();
                drop(t);
                if batch {
                    app.compress_batch();
                } else {
                    app.compress(None, Mode::Fit, false);
                }
            }
            View::Busy => next(&mut t, Step::WaitDone),
            View::Error => {
                t.failed = Some(format!("couldn't open: {}", app.ui().get_error_text()));
                drop(t);
                finish(test);
            }
            _ if waited > Duration::from_secs(60) => {
                t.failed = Some("the file never opened".into());
                drop(t);
                finish(test);
            }
            _ => {}
        },
        Step::WaitDone => match app.view() {
            View::Done | View::Idle if waited > Duration::from_secs(1) => {
                println!(
                    "selftest:   done, panel open: {mem:.1} MB (peak so far {:.1} MB)",
                    t.peak
                );
                next(&mut t, Step::Hide);
            }
            View::Error => {
                t.failed = Some(format!("job failed: {}", app.ui().get_error_text()));
                drop(t);
                finish(test);
            }
            _ if waited > JOB_TIMEOUT => {
                t.failed = Some("job timed out".into());
                drop(t);
                finish(test);
            }
            _ => {}
        },
        Step::Hide if waited > Duration::from_secs(2) => {
            app.close();
            next(&mut t, Step::Hidden);
        }
        Step::Idle if waited > IDLE_WATCH => {
            let used = cpu_seconds() - t.idle_cpu;
            let share = used / waited.as_secs_f64();
            println!(
                "selftest: hidden CPU use: {:.2}% of one core ({used:.2}s)",
                share * 100.0
            );
            if share > IDLE_CPU_LIMIT {
                t.failed_idle = Some(format!(
                    "the hidden app used {:.1}% of a CPU core (limit {:.0}%)",
                    share * 100.0,
                    IDLE_CPU_LIMIT * 100.0
                ));
            }
            drop(t);
            finish(test);
        }
        Step::Hidden if waited > Duration::from_secs(4) => {
            let i = t.job % t.files.len();
            t.hidden[i].push(mem);
            println!("selftest:   hidden: {mem:.1} MB");
            t.job += 1;
            next(&mut t, Step::Open);
        }
        _ => {}
    }
}

fn finish(test: &Rc<RefCell<Test>>) {
    let t = test.borrow();
    let mut problems = Vec::new();
    if let Some(f) = &t.failed {
        println!("selftest: FAILED: {f}");
        std::process::exit(2);
    }
    let slack = std::env::var("KOMPACTER_SELFTEST_SLACK")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(40.0);
    if let Some(f) = &t.failed_idle {
        problems.push(f.clone());
    }
    for (file, readings) in t.files.iter().zip(&t.hidden) {
        let (Some(first), Some(last)) = (readings.first(), readings.last()) else {
            continue;
        };
        if last - first > ROUND_SLACK_MB {
            problems.push(format!(
                "{}: hidden memory grew {:.1} MB between rounds ({first:.1} → {last:.1} MB)",
                file.display(),
                last - first
            ));
        }
        if last - t.baseline > slack {
            problems.push(format!(
                "{}: hidden at {last:.1} MB, {:.1} MB over the {:.1} MB baseline",
                file.display(),
                last - t.baseline,
                t.baseline
            ));
        }
    }
    println!(
        "selftest: baseline {:.1} MB, peak {:.1} MB, last hidden {:.1} MB",
        t.baseline,
        t.peak,
        t.hidden
            .iter()
            .filter_map(|r| r.last())
            .next_back()
            .copied()
            .unwrap_or(0.0)
    );
    if problems.is_empty() {
        println!("selftest: OK");
        std::process::exit(0);
    }
    for p in problems {
        println!("selftest: FAILED: {p}");
    }
    std::process::exit(1);
}

/// CPU time this process has used so far (user + system), in seconds.
#[cfg(target_os = "linux")]
fn cpu_seconds() -> f64 {
    // Fields 14 and 15 of /proc/self/stat, in clock ticks (100 per second on Linux),
    // counted after the ")" that ends the process name.
    std::fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|s| {
            let rest = s.rsplit_once(')')?.1;
            let f: Vec<&str> = rest.split_whitespace().collect();
            Some((f.get(11)?.parse::<f64>().ok()? + f.get(12)?.parse::<f64>().ok()?) / 100.0)
        })
        .unwrap_or(0.0)
}

#[cfg(windows)]
fn cpu_seconds() -> f64 {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: plain out-parameters for the current process.
    if unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    } == 0
    {
        return 0.0;
    }
    let ticks =
        |t: FILETIME| (u64::from(t.dwHighDateTime) << 32 | u64::from(t.dwLowDateTime)) as f64;
    // 100 ns units.
    (ticks(kernel) + ticks(user)) / 1e7
}

#[cfg(target_os = "macos")]
fn cpu_seconds() -> f64 {
    // `ps -o time=`: "M:SS.cc" of CPU time.
    let out = std::process::Command::new("/bin/ps")
        .args(["-o", "time=", "-p", &std::process::id().to_string()])
        .output();
    out.ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| {
            let s = s.trim();
            let (m, sec) = s.rsplit_once(':')?;
            Some(m.parse::<f64>().ok()? * 60.0 + sec.parse::<f64>().ok()?)
        })
        .unwrap_or(0.0)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn cpu_seconds() -> f64 {
    0.0
}

/// What a task manager would show as this app's own memory, in MB.
#[cfg(target_os = "linux")]
fn memory_mb() -> f64 {
    // RssAnon: private memory, without shared libraries (like Windows' private working set).
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("RssAnon:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|kb| kb.parse::<f64>().ok())
        })
        .map_or(0.0, |kb| kb / 1024.0)
}

#[cfg(windows)]
fn memory_mb() -> f64 {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // SAFETY: a plain query into a zeroed, correctly sized struct.
    unsafe {
        let mut c: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
        c.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        if GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut c as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS,
            c.cb,
        ) == 0
        {
            return 0.0;
        }
        // Private bytes: what this process itself has committed.
        c.PrivateUsage as f64 / (1024.0 * 1024.0)
    }
}

#[cfg(target_os = "macos")]
fn memory_mb() -> f64 {
    let out = std::process::Command::new("/bin/ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output();
    out.ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<f64>().ok())
        .map_or(0.0, |kb| kb / 1024.0)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn memory_mb() -> f64 {
    0.0
}
