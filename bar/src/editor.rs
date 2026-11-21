//! Visual curve editor window. It edits ~/.config/gust/curve.conf only; gustd
//! picks up the saved file on its own (and rejects a bad one).

use gust::control::EMERGENCY_C;
use gust::curve::{self, Curve, Shape, Speed};
use gust::fanctl::Fan;
use serde_json::{Value, json};
use tao::dpi::LogicalSize;
use tao::event_loop::EventLoopWindowTarget;
use tao::window::{Window, WindowBuilder, WindowId};
use wry::{WebView, WebViewBuilder};

const HTML: &str = include_str!("editor.html");

pub struct Editor {
    window: Window,
    webview: WebView,
}

/// Curve points as the page wants them: % of each fan's range, or auto.
/// RPM points become a % of fan 0's range (the editor works in %).
fn points_json(c: &Curve, fans: &[Fan]) -> Value {
    let f0 = fans.first().copied().unwrap_or(Fan { min: 0.0, max: 1.0 });
    let pts: Vec<Value> = c
        .points
        .iter()
        .map(|(t, s)| match s {
            Speed::Auto => json!({"t": t, "auto": true, "pct": 0}),
            Speed::Max => json!({"t": t, "auto": false, "pct": 100}),
            Speed::Percent(p) => json!({"t": t, "auto": false, "pct": p}),
            Speed::Rpm(r) => {
                let p = ((r - f0.min) / (f0.max - f0.min) * 100.0).clamp(0.0, 100.0).round();
                json!({"t": t, "auto": false, "pct": p})
            }
        })
        .collect();
    Value::Array(pts)
}

impl Editor {
    pub fn open<T: 'static>(
        target: &EventLoopWindowTarget<T>,
        path: &str,
        fans: &[Fan],
        on_ipc: impl Fn(String) + 'static,
    ) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        let c = Curve::parse(&text).map_err(|e| format!("{path}: {e}"))?;
        let init = json!({
            "points": points_json(&c, fans),
            "fans": fans.iter().map(|f| json!({"min": f.min, "max": f.max})).collect::<Vec<_>>(),
            "emergency": EMERGENCY_C,
            "shape": c.shape.name(),
            "average": c.average.unwrap_or(0.0),
        });
        let window = WindowBuilder::new()
            .with_title("gust curve")
            .with_inner_size(LogicalSize::new(820.0, 520.0))
            .with_min_inner_size(LogicalSize::new(560.0, 360.0))
            .build(target)
            .map_err(|e| e.to_string())?;
        let webview = WebViewBuilder::new()
            .with_initialization_script(format!("window.GUST = {init};"))
            .with_html(HTML)
            .with_ipc_handler(move |req| on_ipc(req.into_body()))
            .build(&window)
            .map_err(|e| e.to_string())?;
        window.set_focus();
        Ok(Editor { window, webview })
    }

    pub fn id(&self) -> WindowId {
        self.window.id()
    }

    pub fn focus(&self) {
        self.window.set_focus();
    }

    pub fn push_live(&self, live: &Value) {
        let _ = self.webview.evaluate_script(&format!("window.gustLive && window.gustLive({live})"));
    }

    /// Handle a message from the page. Only "save" exists.
    pub fn handle(&self, msg: &str, path: &str) {
        let (ok, text) = match save(msg, path) {
            Ok(()) => (true, "Saved. gustd picks it up within 2 s.".to_string()),
            Err(e) => (false, format!("Not saved: {e}")),
        };
        let _ = self.webview.evaluate_script(&format!("window.gustSaved({ok}, {})", json!(text)));
    }
}

fn save(msg: &str, path: &str) -> Result<(), String> {
    let v: Value = serde_json::from_str(msg).map_err(|e| e.to_string())?;
    if v["cmd"] != "save" {
        return Err("unknown command".into());
    }
    let mut points = Vec::new();
    for p in v["points"].as_array().ok_or("no points")? {
        let t = p["t"].as_f64().ok_or("bad temperature")?;
        let speed = if p["auto"].as_bool().unwrap_or(false) {
            Speed::Auto
        } else {
            let pct = p["pct"].as_f64().ok_or("bad speed")?;
            if pct >= 100.0 { Speed::Max } else { Speed::Percent(pct.max(0.0)) }
        };
        points.push((t, speed));
    }
    let mut settings: Vec<(&str, String)> = Vec::new();
    if let Some(s) = v["shape"].as_str() {
        settings.push(("shape", Shape::parse(s).ok_or("bad shape")?.name().to_string()));
    }
    if let Some(a) = v["average"].as_f64() {
        if !(0.0..=600.0).contains(&a) {
            return Err("average must be 0 to 600 seconds".into());
        }
        settings.push(("average", format!("{}", a.round())));
    }
    let original = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let out = curve::rewrite_points(&original, &points, &settings).map_err(|e| e.to_string())?;
    // Atomic replace so gustd never reads a half-written file.
    let tmp = format!("{path}.tmp");
    std::fs::write(&tmp, out).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpfile(body: &str) -> String {
        let p = std::env::temp_dir().join(format!("gust-ed-{}-{}.conf", std::process::id(), body.len()));
        std::fs::write(&p, body).unwrap();
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn save_rewrites_points_and_keeps_the_rest() {
        let p = tmpfile("# mine\nsensors = Tp0*\npoint 60 auto\npoint 100 max\n");
        save(r#"{"cmd":"save","points":[{"t":55,"auto":true,"pct":0},{"t":70,"auto":false,"pct":20},{"t":98.5,"auto":false,"pct":100}]}"#, &p).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "# mine\nsensors = Tp0*\npoint 55 auto\npoint 70 20%\npoint 98.5 max\n");
        std::fs::remove_file(p).unwrap();
    }

    #[test]
    fn bad_save_leaves_file_alone() {
        let body = "sensors = Tp0*\npoint 60 auto\n";
        let p = tmpfile(body);
        assert!(save(r#"{"cmd":"save","points":[{"t":80,"auto":false,"pct":50},{"t":70,"auto":true,"pct":0}]}"#, &p).is_err());
        assert!(save(r#"{"cmd":"save","points":[]}"#, &p).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), body);
        std::fs::remove_file(p).unwrap();
    }

    #[test]
    fn save_sets_shape() {
        let p = tmpfile("sensors = Tp0*\npoint 60 auto\nramp_up = 50\n");
        save(r#"{"cmd":"save","points":[{"t":60,"auto":false,"pct":100}],"shape":"log"}"#, &p).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "sensors = Tp0*\npoint 60 max\nshape = log\nramp_up = 50\n");
        std::fs::remove_file(p).unwrap();
    }

    #[test]
    fn save_sets_average() {
        let p = tmpfile("sensors = Tp0*\npoint 60 auto\n# keep\n");
        save(r#"{"cmd":"save","points":[{"t":60,"auto":true,"pct":0}],"shape":"linear","average":20}"#, &p).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "sensors = Tp0*\npoint 60 auto\nshape = linear\naverage = 20\n# keep\n");
        assert!(save(r#"{"cmd":"save","points":[{"t":60,"auto":true,"pct":0}],"average":9000}"#, &p).is_err());
        std::fs::remove_file(p).unwrap();
    }

    #[test]
    fn rpm_points_shown_as_percent() {
        let c = Curve::parse("sensors = T*\npoint 70 3500\n").unwrap();
        let v = points_json(&c, &[Fan { min: 1500.0, max: 5500.0 }]);
        assert_eq!(v[0]["pct"], 50.0);
    }
}
