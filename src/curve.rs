//! Fan curve: a plain text file Kevin edits. Parsing and evaluation only;
//! nothing here talks to the fans.
//!
//! Format (one setting per line, # starts a comment):
//!   sensors = Tp0* TC1* TCDX Tg*     hottest matching sensor drives the fans
//!   point 60 auto                    at 60°C: hand control back to macOS
//!   point 75 30%                     at 75°C: 30% of each fan's min..max range
//!   point 85 2500                    at 85°C: 2500 RPM
//!   point 95 max                     at 95°C and above: full speed
//!   ramp_up = 50                     optional: speed up at most 50 RPM/s
//!   ramp_down = 30                   optional: slow down at most 30 RPM/s
//!   shape = log                      optional: linear (default), log or exp
//!   average = 20                     optional: curve follows the 20 s rolling
//!                                    average temperature (the 100°C override
//!                                    still uses the live reading)
//! `shape` bends every segment between two speeds: log rises fast then levels
//! off, exp stays low then climbs steeply. The points themselves don't move.
//! Between two points the speed ramps linearly. A segment that starts at
//! `auto` stays `auto` until the next point is reached.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Speed {
    Auto,
    Rpm(f64),
    Percent(f64),
    Max,
}

/// How speed moves between two points.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum Shape {
    #[default]
    Linear,
    /// log10(1 + 9f): fast rise, then levels off.
    Log,
    /// (10^f - 1) / 9: the inverse of Log; low, then steep.
    Exp,
}

impl Shape {
    /// Map a 0..1 position within a segment to a 0..1 share of the speed change.
    pub fn apply(self, f: f64) -> f64 {
        let f = f.clamp(0.0, 1.0);
        match self {
            Shape::Linear => f,
            Shape::Log => (1.0 + 9.0 * f).log10(),
            Shape::Exp => (10f64.powf(f) - 1.0) / 9.0,
        }
    }

    pub fn parse(s: &str) -> Option<Shape> {
        match s {
            "linear" => Some(Shape::Linear),
            "log" => Some(Shape::Log),
            "exp" => Some(Shape::Exp),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Shape::Linear => "linear",
            Shape::Log => "log",
            Shape::Exp => "exp",
        }
    }
}

#[derive(Debug)]
pub struct Curve {
    pub sensors: Vec<String>,
    pub points: Vec<(f64, Speed)>,
    /// Max RPM per second when speeding up. None = jump straight there.
    pub ramp_up: Option<f64>,
    /// Max RPM per second when slowing down. None = the built-in default.
    pub ramp_down: Option<f64>,
    pub shape: Shape,
    /// Seconds of rolling average the curve follows. None = live reading.
    pub average: Option<f64>,
}

#[derive(Debug)]
pub struct ParseError {
    pub line: usize,
    pub msg: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.msg)
    }
}

fn parse_speed(s: &str) -> Option<Speed> {
    match s {
        "auto" => Some(Speed::Auto),
        "max" => Some(Speed::Max),
        _ if s.ends_with('%') => {
            let p: f64 = s.trim_end_matches('%').parse().ok()?;
            (0.0..=100.0).contains(&p).then_some(Speed::Percent(p))
        }
        _ => s.parse::<f64>().ok().filter(|r| *r >= 0.0).map(Speed::Rpm),
    }
}

impl Curve {
    pub fn parse(text: &str) -> Result<Curve, ParseError> {
        let mut sensors = Vec::new();
        let mut points: Vec<(f64, Speed)> = Vec::new();
        let (mut ramp_up, mut ramp_down) = (None, None);
        let mut shape = Shape::Linear;
        let mut average = None;
        for (i, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap().trim();
            let err = |msg: &str| ParseError { line: i + 1, msg: msg.to_string() };
            if line.is_empty() {
                continue;
            }
            if let Some((name, val)) = line.split_once('=')
                && matches!(name.trim(), "ramp_up" | "ramp_down")
            {
                let v: f64 = val.trim().parse().map_err(|_| err("ramp must be a number of RPM per second"))?;
                if !(v > 0.0 && v.is_finite()) {
                    return Err(err("ramp must be more than 0 RPM per second"));
                }
                if name.trim() == "ramp_up" { ramp_up = Some(v) } else { ramp_down = Some(v) }
            } else if let Some((name, val)) = line.split_once('=')
                && name.trim() == "average"
            {
                let v: f64 = val.trim().parse().map_err(|_| err("average must be a number of seconds"))?;
                if !(0.0..=600.0).contains(&v) {
                    return Err(err("average must be between 0 and 600 seconds"));
                }
                average = (v > 0.0).then_some(v);
            } else if let Some((name, val)) = line.split_once('=')
                && name.trim() == "shape"
            {
                shape = Shape::parse(val.trim()).ok_or_else(|| err("shape must be linear, log or exp"))?;
            } else if let Some(rest) = line.strip_prefix("sensors") {
                let rest = rest.trim_start().strip_prefix('=').ok_or_else(|| err("expected `sensors = ...`"))?;
                sensors = rest.split_whitespace().map(String::from).collect();
            } else if let Some(rest) = line.strip_prefix("point") {
                let parts: Vec<&str> = rest.split_whitespace().collect();
                let [t, s] = parts[..] else { return Err(err("expected `point <temp> <speed>`")) };
                let t: f64 = t.parse().map_err(|_| err("temperature must be a number"))?;
                let s = parse_speed(s).ok_or_else(|| err("speed must be auto, max, an RPM, or a percent like 40%"))?;
                if points.last().is_some_and(|(pt, _)| t <= *pt) {
                    return Err(err("points must go up in temperature"));
                }
                points.push((t, s));
            } else {
                return Err(err("unknown setting (expected `sensors =`, `point`, `shape =`, `average =`, `ramp_up =` or `ramp_down =`)"));
            }
        }
        if sensors.is_empty() {
            return Err(ParseError { line: 0, msg: "no `sensors =` line".into() });
        }
        if points.is_empty() {
            return Err(ParseError { line: 0, msg: "no `point` lines".into() });
        }
        Ok(Curve { sensors, points, ramp_up, ramp_down, shape, average })
    }

    pub fn matches(&self, key: &str) -> bool {
        self.sensors.iter().any(|p| match p.strip_suffix('*') {
            Some(prefix) => key.starts_with(prefix),
            None => key == p,
        })
    }

    /// Target for one fan at `temp`. None = auto (macOS in charge).
    pub fn target(&self, temp: f64, fan_min: f64, fan_max: f64) -> Option<f64> {
        let rpm = |s: Speed| match s {
            Speed::Auto => None,
            Speed::Max => Some(fan_max),
            Speed::Rpm(r) => Some(r),
            Speed::Percent(p) => Some(fan_min + (fan_max - fan_min) * p / 100.0),
        };
        let clamp = |r: f64| r.clamp(fan_min, fan_max);
        let first = self.points[0];
        if temp <= first.0 {
            return rpm(first.1).map(clamp);
        }
        for w in self.points.windows(2) {
            let ((t0, s0), (t1, s1)) = (w[0], w[1]);
            if temp < t1 {
                let (Some(r0), Some(r1)) = (rpm(s0), rpm(s1)) else {
                    // Auto on the low side holds until the next point; auto on the
                    // high side (odd, but legal) takes over once we pass t0.
                    return if s0 == Speed::Auto { None } else { rpm(s0).map(clamp).filter(|_| s1 != Speed::Auto) };
                };
                return Some(clamp(r0 + (r1 - r0) * self.shape.apply((temp - t0) / (t1 - t0))));
            }
        }
        rpm(self.points.last().unwrap().1).map(clamp)
    }
}

impl fmt::Display for Speed {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Speed::Auto => write!(f, "auto"),
            Speed::Max => write!(f, "max"),
            Speed::Rpm(r) => write!(f, "{r:.0}"),
            Speed::Percent(p) => write!(f, "{}%", (p * 10.0).round() / 10.0),
        }
    }
}

/// Replace the `point` lines in a curve file with `points`, keeping every
/// other line (comments, `sensors =`, ramps) where it was. New points go where
/// the first old point was, or at the end if there were none. With
/// `settings` (e.g. `[("shape", "log"), ("average", "20")]`), each `name =`
/// line is replaced in place, or added after the points if missing. The
/// result is parsed before it's returned, so a caller can never write a
/// broken file.
pub fn rewrite_points(original: &str, points: &[(f64, Speed)], settings: &[(&str, String)]) -> Result<String, ParseError> {
    let mut block: String = points.iter().map(|(t, s)| format!("point {} {s}\n", (t * 10.0).round() / 10.0)).collect();
    for (name, val) in settings {
        if !original.lines().any(|l| setting_name(l) == Some(name)) {
            block.push_str(&format!("{name} = {val}\n"));
        }
    }
    let mut out = String::new();
    let mut placed = false;
    for line in original.lines() {
        if let Some((name, val)) = settings.iter().find(|(n, _)| setting_name(line) == Some(n)) {
            out.push_str(&format!("{name} = {val}\n"));
        } else if line.trim_start().starts_with("point") {
            if !placed {
                out.push_str(&block);
                placed = true;
            }
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !placed {
        out.push_str(&block);
    }
    Curve::parse(&out)?;
    Ok(out)
}

/// `Some("shape")` for a `shape = ...` line, etc.
fn setting_name(line: &str) -> Option<&str> {
    line.split('#').next().unwrap().split_once('=').map(|(n, _)| n.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_keeps_comments_and_sensors() {
        let orig = "# mine\nsensors = Tp0*\n# pts\npoint 60 auto\npoint 90 max\n# end\n";
        let out = rewrite_points(orig, &[(55.0, Speed::Auto), (70.0, Speed::Percent(25.0)), (99.5, Speed::Max)], &[]).unwrap();
        assert_eq!(out, "# mine\nsensors = Tp0*\n# pts\npoint 55 auto\npoint 70 25%\npoint 99.5 max\n# end\n");
    }

    #[test]
    fn rewrite_refuses_bad_points() {
        assert!(rewrite_points("sensors = T*\n", &[(70.0, Speed::Max), (60.0, Speed::Auto)], &[]).is_err());
        assert!(rewrite_points("sensors = T*\n", &[], &[]).is_err());
    }

    const C: &str = "sensors = Tp0* TCDX\npoint 60 auto\npoint 70 0%\npoint 90 100%\npoint 95 max\n";

    #[test]
    fn ramps_and_clamps() {
        let c = Curve::parse(C).unwrap();
        assert_eq!(c.target(50.0, 1500.0, 5500.0), None);
        assert_eq!(c.target(65.0, 1500.0, 5500.0), None);
        assert_eq!(c.target(70.0, 1500.0, 5500.0), Some(1500.0));
        assert_eq!(c.target(80.0, 1500.0, 5500.0), Some(3500.0));
        assert_eq!(c.target(120.0, 1500.0, 5500.0), Some(5500.0));
    }

    #[test]
    fn rpm_points_are_clamped_to_fan_range() {
        let c = Curve::parse("sensors = T*\npoint 50 100\npoint 90 9000\n").unwrap();
        assert_eq!(c.target(40.0, 1500.0, 5500.0), Some(1500.0));
        assert_eq!(c.target(95.0, 1500.0, 5500.0), Some(5500.0));
    }

    #[test]
    fn shapes() {
        let lin = Curve::parse("sensors = T*\npoint 60 0%\npoint 100 100%\n").unwrap();
        let log = Curve::parse("sensors = T*\npoint 60 0%\npoint 100 100%\nshape = log\n").unwrap();
        let exp = Curve::parse("sensors = T*\npoint 60 0%\npoint 100 100%\nshape = exp\n").unwrap();
        let at = |c: &Curve, t| c.target(t, 0.0, 1000.0).unwrap().round();
        // Ends don't move; the middle bends.
        for c in [&lin, &log, &exp] {
            assert_eq!((at(c, 60.0), at(c, 100.0)), (0.0, 1000.0));
        }
        assert_eq!(at(&lin, 80.0), 500.0);
        assert_eq!(at(&log, 80.0), 740.0); // log10(5.5)
        assert_eq!(at(&exp, 80.0), 240.0); // (sqrt(10)-1)/9
        assert!(Curve::parse("sensors = T*\npoint 60 0%\nshape = curvy\n").is_err());
    }

    #[test]
    fn rewrite_sets_shape() {
        let a = rewrite_points("sensors = T*\npoint 60 auto\n", &[(60.0, Speed::Max)], &[("shape", "log".into())]).unwrap();
        assert_eq!(a, "sensors = T*\npoint 60 max\nshape = log\n");
        let b = rewrite_points(&a, &[(60.0, Speed::Max)], &[("shape", "exp".into())]).unwrap();
        assert_eq!(b, "sensors = T*\npoint 60 max\nshape = exp\n");
        let c = rewrite_points(&b, &[(70.0, Speed::Max)], &[]).unwrap();
        assert!(c.contains("shape = exp"));
    }

    #[test]
    fn average_parses_and_rewrites() {
        let c = Curve::parse("sensors = T*\npoint 60 max\naverage = 20\n").unwrap();
        assert_eq!(c.average, Some(20.0));
        assert_eq!(Curve::parse("sensors = T*\npoint 60 max\naverage = 0\n").unwrap().average, None);
        assert!(Curve::parse("sensors = T*\npoint 60 max\naverage = 900\n").is_err());
        let out = rewrite_points("sensors = T*\npoint 60 max\naverage = 20\n", &[(60.0, Speed::Max)], &[("average", "45".into())]).unwrap();
        assert_eq!(out, "sensors = T*\npoint 60 max\naverage = 45\n");
    }

    #[test]
    fn ramps_parse() {
        let c = Curve::parse("sensors = T*\npoint 70 max\nramp_up = 50\nramp_down=30\n").unwrap();
        assert_eq!((c.ramp_up, c.ramp_down), (Some(50.0), Some(30.0)));
        let d = Curve::parse("sensors = T*\npoint 70 max\n").unwrap();
        assert_eq!((d.ramp_up, d.ramp_down), (None, None));
    }

    #[test]
    fn matching() {
        let c = Curve::parse(C).unwrap();
        assert!(c.matches("Tp0a") && c.matches("TCDX") && !c.matches("TCDY") && !c.matches("Tg05"));
    }

    #[test]
    fn rejects_bad_files() {
        assert!(Curve::parse("point 60 auto\n").is_err());
        assert!(Curve::parse("sensors = T*\npoint 70 auto\npoint 60 max\n").is_err());
        assert!(Curve::parse("sensors = T*\npoint 70 fast\n").is_err());
        assert!(Curve::parse("sensors = T*\npoint 70 150%\n").is_err());
        assert!(Curve::parse("sensors = T*\npoint 70 max\nramp_up = 0\n").is_err());
        assert!(Curve::parse("sensors = T*\npoint 70 max\nramp_down = fast\n").is_err());
    }
}
