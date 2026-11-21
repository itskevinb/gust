//! gust-bar: menu bar readout for gust. Read-only: it reads SMC sensors and
//! gustd's status file, and never writes to the fans.

use gust::sensors;
use gust::smc::Smc;
use serde_json::Value;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
use tray_icon::TrayIconBuilder;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};

const REFRESH: Duration = Duration::from_secs(2);
/// Status older than this means gustd isn't running.
const STALE_S: u64 = 10;

// Sensor groups from the 2026-11-21 load test (see dev notes).
const CPU: &[&str] = &["Tp0", "TC1", "TCDX"];
const GPU: &[&str] = &["Tg", "TC2", "TC3"];

fn keys_with(all: &[String], prefixes: &[&str]) -> Vec<String> {
    all.iter().filter(|k| prefixes.iter().any(|p| k.starts_with(p))).cloned().collect()
}

fn curve_path() -> String {
    format!("{}/.config/gust/curve.conf", std::env::var("HOME").unwrap_or_default())
}

fn now_s() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// gustd status, or None if missing or stale.
fn read_status() -> Option<Value> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(gust::STATUS_PATH).ok()?).ok()?;
    let t = v.get("time")?.as_u64()?;
    (now_s().saturating_sub(t) <= STALE_S).then_some(v)
}

fn fmt_rpm(r: f64) -> String {
    if r >= 1000.0 { format!("{:.1}k", r / 1000.0) } else { format!("{r:.0}") }
}

struct Ui {
    state: MenuItem,
    cpu: MenuItem,
    gpu: MenuItem,
    fans: Vec<MenuItem>,
    curve: MenuItem,
    edit: MenuItem,
    log: MenuItem,
    quit: MenuItem,
}

enum UserEvent {
    Menu(MenuEvent),
}

fn main() {
    let smc = Smc::open().expect("cannot open SMC");
    let all = smc.keys().unwrap_or_default();
    let cpu_keys = keys_with(&all, CPU);
    let gpu_keys = keys_with(&all, GPU);
    let fan_count = smc.read("FNum").ok().and_then(|v| v.as_f64()).unwrap_or(0.0) as usize;

    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    event_loop.set_activation_policy(ActivationPolicy::Accessory); // no Dock icon
    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |e| {
        let _ = proxy.send_event(UserEvent::Menu(e));
    }));

    let item = |t: &str| MenuItem::new(t, false, None);
    let ui = Ui {
        state: item("…"),
        cpu: item("CPU …"),
        gpu: item("GPU …"),
        fans: (0..fan_count).map(|i| item(&format!("Fan {i} …"))).collect(),
        curve: item(""),
        edit: MenuItem::new("Edit curve…", true, None),
        log: MenuItem::new("Open gustd log", true, None),
        quit: MenuItem::new("Quit menu bar app", true, None),
    };
    let menu = Menu::new();
    menu.append(&ui.state).unwrap();
    menu.append(&PredefinedMenuItem::separator()).unwrap();
    menu.append(&ui.cpu).unwrap();
    menu.append(&ui.gpu).unwrap();
    for f in &ui.fans {
        menu.append(f).unwrap();
    }
    menu.append(&ui.curve).unwrap();
    menu.append(&PredefinedMenuItem::separator()).unwrap();
    menu.append(&ui.edit).unwrap();
    menu.append(&ui.log).unwrap();
    menu.append(&PredefinedMenuItem::separator()).unwrap();
    menu.append(&ui.quit).unwrap();

    let mut tray = None;
    let mut menu = Some(menu);

    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::WaitUntil(Instant::now() + REFRESH);
        match event {
            Event::NewEvents(StartCause::Init) => {
                // tray-icon: create the icon only once the loop is running.
                tray = Some(
                    TrayIconBuilder::new()
                        .with_menu(Box::new(menu.take().unwrap()))
                        .with_title("gust")
                        .with_tooltip("gust fan control")
                        .build()
                        .expect("cannot create menu bar item"),
                );
                refresh(&smc, &cpu_keys, &gpu_keys, &ui, tray.as_ref().unwrap());
            }
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                if let Some(t) = &tray {
                    refresh(&smc, &cpu_keys, &gpu_keys, &ui, t);
                }
            }
            Event::UserEvent(UserEvent::Menu(e)) => {
                if e.id == ui.quit.id() {
                    tray.take();
                    *flow = ControlFlow::Exit;
                } else if e.id == ui.edit.id() {
                    let _ = std::process::Command::new("open").args(["-t", &curve_path()]).spawn();
                } else if e.id == ui.log.id() {
                    let _ = std::process::Command::new("open").args(["-a", "Console", "/var/log/gustd.log"]).spawn();
                }
            }
            _ => {}
        }
    });
}

fn refresh(smc: &Smc, cpu_keys: &[String], gpu_keys: &[String], ui: &Ui, tray: &tray_icon::TrayIcon) {
    let cpu = sensors::hottest(smc, cpu_keys);
    let gpu = sensors::hottest(smc, gpu_keys);
    let show = |name: &str, r: &Option<(String, f64, usize)>| {
        r.as_ref().map_or(format!("{name}: no reading"), |(k, c, _)| format!("{name}  {c:.0}°C   ({k})"))
    };
    ui.cpu.set_text(show("CPU", &cpu));
    ui.gpu.set_text(show("GPU", &gpu));

    let status = read_status();
    let hot = [cpu.as_ref().map(|r| r.1), gpu.as_ref().map(|r| r.1)].into_iter().flatten().fold(f64::NAN, f64::max);

    for (i, item) in ui.fans.iter().enumerate() {
        let rpm = smc.read(&format!("F{i}Ac")).ok().and_then(|v| v.as_f64()).unwrap_or(0.0);
        let target = status
            .as_ref()
            .and_then(|s| s["fans"].get(i))
            .and_then(|f| f["target"].as_f64());
        item.set_text(match target {
            Some(t) => format!("Fan {i}  {rpm:.0} RPM   (gust target {t:.0})"),
            None => format!("Fan {i}  {rpm:.0} RPM"),
        });
    }

    let max_rpm = (0..ui.fans.len())
        .filter_map(|i| smc.read(&format!("F{i}Ac")).ok().and_then(|v| v.as_f64()))
        .fold(0.0, f64::max);
    let temp = if hot.is_finite() { format!("{hot:.0}°") } else { "–°".into() };

    let (state, title) = match &status {
        None => ("⚠︎ gustd not running: macOS controls the fans".to_string(), format!("{temp} ⚠︎")),
        Some(s) if s["mode"] == "auto" => ("macOS auto (curve says auto)".to_string(), format!("{temp} auto")),
        Some(_) => ("gust is controlling the fans".to_string(), format!("{temp} {}", fmt_rpm(max_rpm))),
    };
    ui.state.set_text(state);
    ui.curve.set_text(match status.as_ref().and_then(|s| s["curve"].as_str()) {
        Some("ok") | None => "Curve OK".to_string(),
        Some(e) => format!("⚠︎ curve edit rejected: {e}"),
    });
    tray.set_title(Some(title));
}
