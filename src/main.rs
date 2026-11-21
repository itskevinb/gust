use gust::{curve, smc};

use gust::smc::Smc;
use std::collections::BTreeMap;
use std::process::ExitCode;

type Res = Result<(), Box<dyn std::error::Error>>;

const USAGE: &str = "gust: Mac fan and temperature tool. This CLI never writes to the fans; gustd does.

usage:
  gust fans          fan RPM (actual, min, max, target) and mode
  gust temps         every temperature sensor, grouped by key prefix
  gust read KEY...   raw value of specific SMC keys (e.g. gust read F0Ac)
  gust keys          dump every SMC key with its type and value
  gust snap          KEY VALUE lines for every temp + fan RPM (for logging)
  gust status        what gustd is doing right now
  gust curve [FILE]  dry run: what the curve would set the fans to right now
  gust curve --table [FILE]  the curve as a temp -> RPM table
                     (FILE defaults to ~/.config/gust/curve.conf)";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let smc = match Smc::open() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("gust: {e}");
            return ExitCode::FAILURE;
        }
    };
    let result = match args.first().map(String::as_str) {
        Some("fans") | None => fans(&smc),
        Some("temps") => temps(&smc),
        Some("keys") => keys(&smc),
        Some("snap") => snap(&smc),
        Some("read") if args.len() > 1 => read(&smc, &args[1..]),
        Some("curve") => curve_cmd(&smc, &args[1..]),
        Some("status") => status(),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("gust: {e}");
            ExitCode::FAILURE
        }
    }
}

fn num(smc: &Smc, key: &str) -> Option<f64> {
    smc.read(key).ok()?.as_f64()
}

fn fans(smc: &Smc) -> Res {
    let count = num(smc, "FNum").unwrap_or(0.0) as u32;
    println!("{count} fan(s)");
    println!("{:<4} {:>7} {:>7} {:>7} {:>7}  mode", "fan", "actual", "min", "max", "target");
    for i in 0..count {
        let get = |k: &str| num(smc, &format!("F{i}{k}"));
        let rpm = |v: Option<f64>| v.map_or("-".into(), |v| format!("{v:.0}"));
        let mode = match get("Md") {
            Some(0.0) => "auto".to_string(),
            Some(m) => format!("forced ({m:.0})"),
            None => "?".to_string(),
        };
        println!(
            "{:<4} {:>7} {:>7} {:>7} {:>7}  {}",
            i,
            rpm(get("Ac")),
            rpm(get("Mn")),
            rpm(get("Mx")),
            rpm(get("Tg")),
            mode
        );
    }
    Ok(())
}

fn temps(smc: &Smc) -> Res {
    // Group by the first two characters (Tp, Te, Tg, TB...). Which prefix means
    // which part is not assumed here; we label them once matched against load.
    let mut groups: BTreeMap<String, Vec<(String, f64)>> = BTreeMap::new();
    for key in smc.keys()? {
        if !key.starts_with('T') {
            continue;
        }
        let Ok(v) = smc.read(&key) else { continue };
        let Some(c) = v.as_f64() else { continue };
        // Skip unpopulated or nonsense sensors.
        if !(1.0..=130.0).contains(&c) || matches!(v.kind.as_str(), "ui8 " | "ui16" | "ui32" | "flag") {
            continue;
        }
        groups.entry(key[..2].to_string()).or_default().push((key, c));
    }
    for (prefix, sensors) in &groups {
        let max = sensors.iter().map(|s| s.1).fold(f64::MIN, f64::max);
        let avg = sensors.iter().map(|s| s.1).sum::<f64>() / sensors.len() as f64;
        println!("{prefix}*  {} sensor(s)  avg {avg:5.1}°C  max {max:5.1}°C", sensors.len());
        let line: Vec<String> = sensors.iter().map(|(k, c)| format!("{k} {c:.1}")).collect();
        println!("     {}", line.join("  "));
    }
    Ok(())
}

/// One line per numeric T* key and fan RPM: "KEY VALUE". For logging.
fn snap(smc: &Smc) -> Res {
    for key in smc.keys()? {
        if !(key.starts_with('T') || (key.starts_with('F') && key.ends_with("Ac"))) {
            continue;
        }
        if let Ok(v) = smc.read(&key) {
            if (v.kind == "flt " || v.kind == "sp78" || v.kind == "fpe2")
                && let Some(n) = v.as_f64() {
                    println!("{key} {n:.2}");
                }
        }
    }
    Ok(())
}

fn read(smc: &Smc, keys: &[String]) -> Res {
    for key in keys {
        if key.len() != 4 {
            eprintln!("{key:?}: SMC keys are 4 characters");
            continue;
        }
        match smc.read(key) {
            Ok(v) => print_value(&v),
            Err(e) => println!("{key}  {e}"),
        }
    }
    Ok(())
}

fn keys(smc: &Smc) -> Res {
    for key in smc.keys()? {
        match smc.read(&key) {
            Ok(v) => print_value(&v),
            Err(e) => println!("{key}  {e}"),
        }
    }
    Ok(())
}

fn print_value(v: &smc::Value) {
    let hex: String = v.bytes.iter().map(|b| format!("{b:02x}")).collect();
    match v.as_f64() {
        Some(n) => println!("{}  [{}]  {n}  ({hex})", v.key, v.kind),
        None => println!("{}  [{}]  {hex}", v.key, v.kind),
    }
}

fn default_curve_path() -> String {
    format!("{}/.config/gust/curve.conf", std::env::var("HOME").unwrap_or_default())
}

fn fan_ranges(smc: &Smc) -> Vec<(f64, f64)> {
    let count = num(smc, "FNum").unwrap_or(0.0) as u32;
    (0..count)
        .filter_map(|i| Some((num(smc, &format!("F{i}Mn"))?, num(smc, &format!("F{i}Mx"))?)))
        .collect()
}

/// Dry run only. Reads sensors and fan ranges, prints what the curve wants.
fn curve_cmd(smc: &Smc, args: &[String]) -> Res {
    let table = args.iter().any(|a| a == "--table");
    let path = args.iter().find(|a| !a.starts_with("--")).cloned().unwrap_or_else(default_curve_path);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
    let curve = curve::Curve::parse(&text).map_err(|e| format!("{path}: {e}"))?;
    let fans = fan_ranges(smc);
    let show = |t: Option<f64>| t.map_or("auto".to_string(), |r| format!("{r:.0}"));

    if table {
        print!("{:>6}", "temp");
        for i in 0..fans.len() {
            print!("  {:>6}", format!("fan{i}"));
        }
        println!();
        for t in (40..=105).step_by(5) {
            print!("{:>5}°", t);
            for &(mn, mx) in &fans {
                print!("  {:>6}", show(curve.target(t as f64, mn, mx)));
            }
            println!();
        }
        return Ok(());
    }

    let mut hottest: Option<(String, f64)> = None;
    let mut matched = 0;
    for key in smc.keys()? {
        if !curve.matches(&key) {
            continue;
        }
        let Some(c) = smc.read(&key).ok().and_then(|v| v.as_f64()) else { continue };
        if !(1.0..=130.0).contains(&c) {
            continue;
        }
        matched += 1;
        if hottest.as_ref().is_none_or(|h| c > h.1) {
            hottest = Some((key, c));
        }
    }
    let Some((key, temp)) = hottest else {
        return Err(format!("no sensors match `{}`", curve.sensors.join(" ")).into());
    };
    println!("{matched} sensors match; hottest is {key} at {temp:.1}°C");
    for (i, &(mn, mx)) in fans.iter().enumerate() {
        let now = num(smc, &format!("F{i}Ac")).unwrap_or(0.0);
        println!("fan {i}: curve wants {:>5} RPM   (now {now:.0}, range {mn:.0}-{mx:.0})", show(curve.target(temp, mn, mx)));
    }
    println!("dry run: nothing was written to the fans");
    Ok(())
}

fn status() -> Res {
    match std::fs::read_to_string(gust::STATUS_PATH) {
        Ok(s) => {
            println!("{}", s.trim());
            Ok(())
        }
        Err(e) => Err(format!("{}: {e} (is gustd running?)", gust::STATUS_PATH).into()),
    }
}
