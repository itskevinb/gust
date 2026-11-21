mod smc;

use smc::Smc;
use std::collections::BTreeMap;
use std::process::ExitCode;

const USAGE: &str = "gust: read-only Mac fan and temperature reader

usage:
  gust fans          fan RPM (actual, min, max, target) and mode
  gust temps         every temperature sensor, grouped by key prefix
  gust read KEY...   raw value of specific SMC keys (e.g. gust read F0Ac)
  gust keys          dump every SMC key with its type and value";

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
        Some("read") if args.len() > 1 => read(&smc, &args[1..]),
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

fn fans(smc: &Smc) -> Result<(), smc::Error> {
    let count = num(smc, "FNum").unwrap_or(0.0) as u32;
    println!("{count} fan(s)");
    println!("{:<4} {:>7} {:>7} {:>7} {:>7}  mode", "fan", "actual", "min", "max", "target");
    for i in 0..count {
        let get = |k: &str| num(smc, &format!("F{i}{k}"));
        let rpm = |v: Option<f64>| v.map_or("-".into(), |v| format!("{v:.0}"));
        let mode = match get("Md") {
            Some(m) if m == 0.0 => "auto".to_string(),
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

fn temps(smc: &Smc) -> Result<(), smc::Error> {
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

fn read(smc: &Smc, keys: &[String]) -> Result<(), smc::Error> {
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

fn keys(smc: &Smc) -> Result<(), smc::Error> {
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
