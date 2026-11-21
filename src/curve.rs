//! Fan curve: a plain text file Kevin edits. Parsing and evaluation only;
//! nothing here talks to the fans.
//!
//! Format (one setting per line, # starts a comment):
//!   sensors = Tp0* TC1* TCDX Tg*     hottest matching sensor drives the fans
//!   point 60 auto                    at 60°C: hand control back to macOS
//!   point 75 30%                     at 75°C: 30% of each fan's min..max range
//!   point 85 2500                    at 85°C: 2500 RPM
//!   point 95 max                     at 95°C and above: full speed
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

#[derive(Debug)]
pub struct Curve {
    pub sensors: Vec<String>,
    pub points: Vec<(f64, Speed)>,
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
        for (i, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap().trim();
            let err = |msg: &str| ParseError { line: i + 1, msg: msg.to_string() };
            if line.is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix("sensors") {
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
                return Err(err("unknown setting (expected `sensors =` or `point`)"));
            }
        }
        if sensors.is_empty() {
            return Err(ParseError { line: 0, msg: "no `sensors =` line".into() });
        }
        if points.is_empty() {
            return Err(ParseError { line: 0, msg: "no `point` lines".into() });
        }
        Ok(Curve { sensors, points })
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
                return Some(clamp(r0 + (r1 - r0) * (temp - t0) / (t1 - t0)));
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
/// other line (comments, `sensors =`) where it was. New points go where the
/// first old point was, or at the end if there were none. The result is
/// parsed before it's returned, so a caller can never write a broken file.
pub fn rewrite_points(original: &str, points: &[(f64, Speed)]) -> Result<String, ParseError> {
    let block: String = points.iter().map(|(t, s)| format!("point {} {s}\n", (t * 10.0).round() / 10.0)).collect();
    let mut out = String::new();
    let mut placed = false;
    for line in original.lines() {
        if line.trim_start().starts_with("point") {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_keeps_comments_and_sensors() {
        let orig = "# mine\nsensors = Tp0*\n# pts\npoint 60 auto\npoint 90 max\n# end\n";
        let out = rewrite_points(orig, &[(55.0, Speed::Auto), (70.0, Speed::Percent(25.0)), (99.5, Speed::Max)]).unwrap();
        assert_eq!(out, "# mine\nsensors = Tp0*\n# pts\npoint 55 auto\npoint 70 25%\npoint 99.5 max\n# end\n");
    }

    #[test]
    fn rewrite_refuses_bad_points() {
        assert!(rewrite_points("sensors = T*\n", &[(70.0, Speed::Max), (60.0, Speed::Auto)]).is_err());
        assert!(rewrite_points("sensors = T*\n", &[]).is_err());
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
    }
}
