//! gustd: the root daemon that applies the fan curve.
//!
//!   gustd --curve PATH            run (needs root; launchd starts it)
//!   gustd --curve PATH --dry-run  same loop, prints decisions, never writes
//!
//! Fail-safes, in order of how often they matter:
//! - startup hands fans back to macOS first, in case a previous run was killed;
//! - SIGTERM/SIGINT/SIGHUP/SIGQUIT -> release fans, exit 0;
//! - panic -> panic hook releases on a fresh SMC connection;
//! - sleep/wake (wall clock jumps) -> release, then pick up again;
//! - sensors unreadable for 3 ticks in a row -> release until they come back;
//! - SMC writes failing 3 ticks in a row -> release and exit 1 (launchd retries);
//! - 100°C or more on any curve sensor -> every fan at max, whatever the curve says.
//!
//! SIGKILL cannot be caught: the next start releases, and launchd's KeepAlive
//! restarts gustd right away.

use gust::control::{Controller, Decision};
use gust::curve::Curve;
use gust::fanctl::{self, FanControl};
use gust::sensors;
use gust::smc::Smc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const TICK: Duration = Duration::from_secs(2);
const LOG_STEP_RPM: f64 = 500.0;
const LOG_HEARTBEAT: Duration = Duration::from_secs(300);

static STOP: AtomicBool = AtomicBool::new(false);

unsafe extern "C" {
    fn geteuid() -> u32;
    fn signal(sig: i32, handler: usize) -> usize;
}

extern "C" fn on_signal(_: i32) {
    STOP.store(true, Ordering::SeqCst);
}

fn log(msg: &str) {
    let t = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    eprintln!("{t} gustd: {msg}");
}

fn json_str(s: &str) -> String {
    let mut o = String::from('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => o.push(' '),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

struct LoadedCurve {
    curve: Curve,
    keys: Vec<String>,
    mtime: Option<SystemTime>,
}

fn mtime(path: &str) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

fn load_curve(smc: &Smc, path: &str) -> Result<LoadedCurve, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let curve = Curve::parse(&text).map_err(|e| format!("{path}: {e}"))?;
    let keys = sensors::matching_keys(smc, &curve).map_err(|e| e.to_string())?;
    if keys.is_empty() {
        return Err(format!("{path}: no SMC sensors match `{}`", curve.sensors.join(" ")));
    }
    Ok(LoadedCurve { curve, keys, mtime: mtime(path) })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dry = args.iter().any(|a| a == "--dry-run");
    let Some(path) = args.iter().position(|a| a == "--curve").and_then(|i| args.get(i + 1)).cloned() else {
        eprintln!("usage: gustd --curve PATH [--dry-run]");
        std::process::exit(2);
    };
    if !dry && unsafe { geteuid() } != 0 {
        eprintln!("gustd: must run as root (or use --dry-run)");
        std::process::exit(2);
    }
    for sig in [1, 2, 3, 15] {
        unsafe { signal(sig, on_signal as *const () as usize) };
    }
    if !dry {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = fanctl::emergency_release();
            prev(info);
        }));
    }
    std::process::exit(run(&path, dry));
}

fn run(path: &str, dry: bool) -> i32 {
    let smc = match Smc::open() {
        Ok(s) => s,
        Err(e) => {
            log(&format!("cannot open SMC: {e}"));
            return 1;
        }
    };
    let mut fc = if dry {
        None
    } else {
        // Clean slate: a previous gustd (or another fan app) may have left fans forced.
        if let Err(e) = fanctl::emergency_release() {
            log(&format!("startup release failed: {e}"));
        }
        match FanControl::open() {
            Ok(f) => Some(f),
            Err(e) => {
                log(&format!("cannot open fan control: {e}"));
                return 1;
            }
        }
    };
    let fans = match fanctl::read_fans(&smc) {
        Ok(f) if !f.is_empty() => f,
        Ok(_) => {
            log("this Mac reports no fans");
            return 1;
        }
        Err(e) => {
            log(&format!("cannot read fans: {e}"));
            return 1;
        }
    };
    let mut loaded = match load_curve(&smc, path) {
        Ok(l) => l,
        Err(e) => {
            log(&format!("no usable curve, leaving fans on auto: {e}"));
            return 1;
        }
    };
    let mut curve_err: Option<String> = None;
    log(&format!(
        "started{}; {} fans, {} sensors from {path}",
        if dry { " (dry run)" } else { "" },
        fans.len(),
        loaded.keys.len()
    ));

    let start = Instant::now();
    let mut ctl = Controller::new(fans.clone());
    let mut last_wall = SystemTime::now();
    let mut read_fails = 0;
    let mut write_fails = 0;
    let mut released = true;
    let mut last_logged: Option<Vec<f64>> = None;
    let mut last_log_at = Instant::now();

    while !STOP.load(Ordering::SeqCst) {
        // Sleep/wake: wall clock jumped well past one tick.
        let now_wall = SystemTime::now();
        if now_wall.duration_since(last_wall).unwrap_or_default() > TICK * 5 {
            log("wake from sleep detected; releasing and starting over");
            if let Some(f) = fc.as_mut() {
                let _ = f.release();
            }
            ctl.reset();
            released = true;
        }
        last_wall = now_wall;

        // Pick up curve edits.
        if mtime(path) != loaded.mtime {
            match load_curve(&smc, path) {
                Ok(l) => {
                    log(&format!("curve reloaded: {} sensors", l.keys.len()));
                    loaded = l;
                    curve_err = None;
                }
                Err(e) => {
                    log(&format!("curve edit rejected, keeping the previous curve: {e}"));
                    loaded.mtime = mtime(path);
                    curve_err = Some(e);
                }
            }
        }

        let reading = sensors::hottest(&smc, &loaded.keys);
        let decision = match &reading {
            Some((_, temp, _)) => {
                read_fails = 0;
                ctl.step(&loaded.curve, *temp, start.elapsed().as_secs_f64())
            }
            None => {
                read_fails += 1;
                if read_fails >= 3 {
                    ctl.reset();
                    Decision::Auto
                } else {
                    // Brief glitch: keep doing whatever we were doing.
                    sleep_tick();
                    continue;
                }
            }
        };

        let mut ok = true;
        match (&decision, fc.as_mut()) {
            (Decision::Auto, Some(f)) if !released => {
                ok = f.release().map_err(|e| log(&format!("release failed: {e}"))).is_ok();
                released = ok;
            }
            (Decision::Set(rpms), Some(f)) => {
                for (i, &r) in rpms.iter().enumerate() {
                    if let Err(e) = f.set_rpm(i, r) {
                        log(&format!("fan {i} write failed: {e}"));
                        ok = false;
                    }
                }
                released = false;
            }
            _ => {}
        }
        if ok {
            write_fails = 0;
        } else {
            write_fails += 1;
            if write_fails >= 3 {
                log("SMC writes keep failing; handing fans back to macOS and exiting");
                if let Some(f) = fc.as_mut() {
                    let _ = f.release();
                }
                return 1;
            }
        }

        // Log mode changes, big moves (>= LOG_STEP_RPM) and a heartbeat; not every tick.
        let now_targets = match &decision {
            Decision::Auto => None,
            Decision::Set(r) => Some(r.clone()),
        };
        let big_move = match (&now_targets, &last_logged) {
            (None, None) => false,
            (Some(a), Some(b)) => a.iter().zip(b).any(|(x, y)| (x - y).abs() >= LOG_STEP_RPM),
            _ => true,
        };
        if dry || big_move || last_log_at.elapsed() >= LOG_HEARTBEAT {
            let mut t = reading.as_ref().map_or("no sensors".into(), |(k, c, _)| format!("{k} {c:.1}°C"));
            if let (Some(win), Some(a)) = (loaded.curve.average, ctl.curve_temp()) {
                t.push_str(&format!(" (avg {win:.0}s {a:.1}°C)"));
            }
            let d = now_targets
                .as_ref()
                .map_or("auto".into(), |r| r.iter().map(|v| format!("{v:.0}")).collect::<Vec<_>>().join("/"));
            log(&format!("{t} -> {d}"));
            last_logged = now_targets;
            last_log_at = Instant::now();
        }
        if !dry {
            let avg = loaded.curve.average.and(ctl.curve_temp());
            write_status(&smc, &fans, &reading, avg, &decision, curve_err.as_deref());
        }
        sleep_tick();
    }

    log("stopping; handing fans back to macOS");
    if let Some(mut f) = fc.take()
        && let Err(e) = f.release()
    {
        log(&format!("release failed: {e}"));
        return 1;
    }
    let _ = std::fs::remove_file(gust::STATUS_PATH);
    0
}

fn sleep_tick() {
    let end = Instant::now() + TICK;
    while Instant::now() < end && !STOP.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn write_status(
    smc: &Smc,
    fans: &[gust::fanctl::Fan],
    reading: &Option<(String, f64, usize)>,
    avg: Option<f64>,
    decision: &Decision,
    curve_err: Option<&str>,
) {
    let t = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let mut fan_json = Vec::new();
    for (i, f) in fans.iter().enumerate() {
        let rpm = smc.read(&format!("F{i}Ac")).ok().and_then(|v| v.as_f64()).unwrap_or(-1.0);
        let target = match decision {
            Decision::Auto => "null".to_string(),
            Decision::Set(r) => format!("{:.0}", r[i]),
        };
        fan_json.push(format!(
            "{{\"fan\":{i},\"rpm\":{rpm:.0},\"target\":{target},\"min\":{:.0},\"max\":{:.0}}}",
            f.min, f.max
        ));
    }
    let (sensor, temp) = reading.as_ref().map_or(("null".into(), "null".into()), |(k, c, _)| (json_str(k), format!("{c:.1}")));
    let mode = if matches!(decision, Decision::Auto) { "auto" } else { "gust" };
    let avg = avg.map_or("null".into(), |a| format!("{a:.1}"));
    let curve = curve_err.map_or("\"ok\"".into(), json_str);
    let body = format!(
        "{{\"time\":{t},\"mode\":\"{mode}\",\"sensor\":{sensor},\"temp\":{temp},\"avg\":{avg},\"curve\":{curve},\"fans\":[{}]}}\n",
        fan_json.join(",")
    );
    let tmp = format!("{}.tmp", gust::STATUS_PATH);
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, gust::STATUS_PATH);
    }
}
