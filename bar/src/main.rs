//! gust-bar: menu bar readout and curve editor for gust. It never writes to
//! the fans; the only file it writes is your curve.conf (via the editor).

mod editor;

use editor::Editor;
use gust::fanctl;
use gust::sensors;
use gust::smc::Smc;
use serde_json::{Value, json};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
use tray_icon::TrayIconBuilder;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};

const REFRESH: Duration = Duration::from_secs(2);
/// Status older than this means gustd isn't running.
const STALE_S: u64 = 10;

// Menu readout groups, measured under CPU and GPU load on an M1 Max. Other
// chips may name sensors differently; `gust temps` lists what yours has.
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
    editor: MenuItem,
    edit: MenuItem,
    log: MenuItem,
    quit: MenuItem,
}

enum UserEvent {
    Menu(MenuEvent),
    Ipc(String),
}

fn main() {
    let smc = Smc::open().expect("cannot open SMC");
    let all = smc.keys().unwrap_or_default();
    let cpu_keys = keys_with(&all, CPU);
    let gpu_keys = keys_with(&all, GPU);
    let fans = fanctl::read_fans(&smc).unwrap_or_default();

    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    event_loop.set_activation_policy(ActivationPolicy::Accessory); // no Dock icon
    let proxy = event_loop.create_proxy();
    let menu_proxy = proxy.clone();
    MenuEvent::set_event_handler(Some(move |e| {
        let _ = menu_proxy.send_event(UserEvent::Menu(e));
    }));

    let item = |t: &str| MenuItem::new(t, false, None);
    let ui = Ui {
        state: item("…"),
        cpu: item("CPU …"),
        gpu: item("GPU …"),
        fans: (0..fans.len()).map(|i| item(&format!("Fan {i} …"))).collect(),
        curve: item(""),
        editor: MenuItem::new("Curve editor…", true, None),
        edit: MenuItem::new("Edit curve file…", true, None),
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
    menu.append(&ui.editor).unwrap();
    menu.append(&ui.edit).unwrap();
    menu.append(&ui.log).unwrap();
    menu.append(&PredefinedMenuItem::separator()).unwrap();
    menu.append(&ui.quit).unwrap();

    let mut tray = None;
    let mut menu = Some(menu);
    let mut ed: Option<Editor> = None;
    let mut next_refresh = Instant::now();

    event_loop.run(move |event, target, flow| {
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
            }
            Event::UserEvent(UserEvent::Menu(e)) => {
                if e.id == ui.quit.id() {
                    ed.take();
                    tray.take();
                    *flow = ControlFlow::Exit;
                    return;
                } else if e.id == ui.editor.id() {
                    match &ed {
                        Some(w) => w.focus(),
                        None => {
                            let p = proxy.clone();
                            match Editor::open(target, &curve_path(), &fans, move |m| {
                                let _ = p.send_event(UserEvent::Ipc(m));
                            }) {
                                Ok(w) => {
                                    ed = Some(w);
                                    next_refresh = Instant::now(); // fill the live line straight away
                                }
                                Err(e) => eprintln!("gust-bar: editor: {e}"),
                            }
                        }
                    }
                } else if e.id == ui.edit.id() {
                    let _ = std::process::Command::new("open").args(["-t", &curve_path()]).spawn();
                } else if e.id == ui.log.id() {
                    let _ = std::process::Command::new("open").args(["-a", "Console", "/var/log/gustd.log"]).spawn();
                }
            }
            Event::UserEvent(UserEvent::Ipc(m)) => {
                if let Some(w) = &ed {
                    w.handle(&m, &curve_path());
                }
            }
            Event::WindowEvent { window_id, event: WindowEvent::CloseRequested, .. } => {
                if ed.as_ref().is_some_and(|w| w.id() == window_id) {
                    ed = None;
                }
            }
            _ => {}
        }
        if Instant::now() >= next_refresh {
            if let Some(t) = &tray {
                let live = refresh(&smc, &cpu_keys, &gpu_keys, &ui, t);
                if let Some(w) = &ed {
                    w.push_live(&live);
                }
            }
            next_refresh = Instant::now() + REFRESH;
        }
        if *flow != ControlFlow::Exit {
            *flow = ControlFlow::WaitUntil(next_refresh);
        }
    });
}

/// Update the menu bar; returns the live reading for the editor.
fn refresh(smc: &Smc, cpu_keys: &[String], gpu_keys: &[String], ui: &Ui, tray: &tray_icon::TrayIcon) -> Value {
    let cpu = sensors::hottest(smc, cpu_keys);
    let gpu = sensors::hottest(smc, gpu_keys);
    let show = |name: &str, r: &Option<(String, f64, usize)>| {
        r.as_ref().map_or(format!("{name}: no reading"), |(k, c, _)| format!("{name}  {c:.0}°C   ({k})"))
    };
    ui.cpu.set_text(show("CPU", &cpu));
    ui.gpu.set_text(show("GPU", &gpu));

    let status = read_status();
    let hottest = [cpu.clone(), gpu.clone()].into_iter().flatten().max_by(|a, b| a.1.total_cmp(&b.1));

    let rpms: Vec<f64> = (0..ui.fans.len())
        .map(|i| smc.read(&format!("F{i}Ac")).ok().and_then(|v| v.as_f64()).unwrap_or(0.0))
        .collect();
    for (i, item) in ui.fans.iter().enumerate() {
        let target = status.as_ref().and_then(|s| s["fans"].get(i)).and_then(|f| f["target"].as_f64());
        item.set_text(match target {
            Some(t) => format!("Fan {i}  {:.0} RPM   (gust target {t:.0})", rpms[i]),
            None => format!("Fan {i}  {:.0} RPM", rpms[i]),
        });
    }

    let max_rpm = rpms.iter().copied().fold(0.0, f64::max);
    let temp = hottest.as_ref().map_or("–°".into(), |h| format!("{:.0}°", h.1));
    let (state, title, mode) = match &status {
        None => ("⚠︎ gustd not running: macOS controls the fans".to_string(), format!("{temp} ⚠︎"), "gustd not running"),
        Some(s) if s["mode"] == "auto" => ("macOS auto (curve says auto)".to_string(), format!("{temp} auto"), "macOS auto"),
        Some(_) => ("gust is controlling the fans".to_string(), format!("{temp} {}", fmt_rpm(max_rpm)), "gust"),
    };
    ui.state.set_text(state);
    let avg = status.as_ref().and_then(|s| s["avg"].as_f64());
    ui.curve.set_text(match (status.as_ref().and_then(|s| s["curve"].as_str()), avg) {
        (Some(e), _) if e != "ok" => format!("⚠︎ curve edit rejected: {e}"),
        (_, Some(a)) => format!("Curve OK · following average {a:.0}°C"),
        _ => "Curve OK".to_string(),
    });
    tray.set_title(Some(title));

    json!({
        "temp": hottest.as_ref().map_or(0.0, |h| h.1),
        "sensor": hottest.as_ref().map_or("none".to_string(), |h| h.0.clone()),
        "mode": mode,
        "avg": avg,
        "rpm": rpms.iter().map(|r| r.round()).collect::<Vec<_>>(),
    })
}
