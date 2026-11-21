//! Pure control logic: temperature in, per-fan targets out. No SMC access,
//! so every rule here is unit tested.

use crate::curve::Curve;
use crate::fanctl::Fan;
use std::collections::VecDeque;

/// At or above this, every fan goes to max whatever the curve says.
pub const EMERGENCY_C: f64 = 100.0;
/// How long the curve must ask for `auto` before we hand back to macOS.
pub const AUTO_HOLD_S: f64 = 30.0;
/// Default ramp down per second when the curve doesn't set `ramp_down`.
/// Ramp up defaults to immediate when the curve doesn't set `ramp_up`.
pub const RAMP_DOWN_RPM_PER_S: f64 = 100.0;

#[derive(Debug, PartialEq)]
pub enum Decision {
    /// macOS controls the fans.
    Auto,
    /// gust sets these RPMs (one per fan).
    Set(Vec<f64>),
}

pub struct Controller {
    fans: Vec<Fan>,
    last: Option<Vec<f64>>,
    auto_since: Option<f64>,
    last_t: Option<f64>,
    /// Recent (t, temp) samples for the curve's `average` window.
    hist: VecDeque<(f64, f64)>,
    avg: Option<f64>,
}

impl Controller {
    pub fn new(fans: Vec<Fan>) -> Self {
        Controller { fans, last: None, auto_since: None, last_t: None, hist: VecDeque::new(), avg: None }
    }

    /// Forget everything, e.g. after wake from sleep or after sensor loss.
    pub fn reset(&mut self) {
        self.release_state();
        self.hist.clear();
        self.avg = None;
    }

    /// Forget fan state only (handing back to macOS); keep the temperature history.
    fn release_state(&mut self) {
        self.last = None;
        self.auto_since = None;
        self.last_t = None;
    }

    /// The temperature the curve used on the last step (the average, if the
    /// curve sets `average`, otherwise the live reading).
    pub fn curve_temp(&self) -> Option<f64> {
        self.avg
    }

    /// Rolling mean of finite readings within the curve's window.
    fn smooth(&mut self, curve: &Curve, temp: f64, t: f64) -> f64 {
        let Some(win) = curve.average else {
            self.hist.clear();
            return temp;
        };
        if temp.is_finite() {
            self.hist.push_back((t, temp));
        }
        while self.hist.front().is_some_and(|(ht, _)| t - ht > win) {
            self.hist.pop_front();
        }
        if self.hist.is_empty() {
            return temp;
        }
        self.hist.iter().map(|h| h.1).sum::<f64>() / self.hist.len() as f64
    }

    /// `temp`: hottest matching sensor right now, °C. `t`: seconds, monotonic.
    /// The curve sees the rolling average (if set); the emergency check always
    /// uses the live reading.
    pub fn step(&mut self, curve: &Curve, temp: f64, t: f64) -> Decision {
        let dt = self.last_t.map_or(0.0, |lt| (t - lt).max(0.0));
        self.last_t = Some(t);
        let smoothed = self.smooth(curve, temp, t);
        self.avg = Some(smoothed);

        let emergency = temp >= EMERGENCY_C || !temp.is_finite();
        let wanted: Vec<Option<f64>> = if emergency {
            self.fans.iter().map(|f| Some(f.max)).collect()
        } else {
            self.fans.iter().map(|f| curve.target(smoothed, f.min, f.max)).collect()
        };

        if wanted.iter().all(Option::is_none) {
            let Some(last) = self.last.clone() else { return Decision::Auto };
            let since = *self.auto_since.get_or_insert(t);
            if t - since >= AUTO_HOLD_S {
                self.release_state();
                return Decision::Auto;
            }
            // Still holding: ease toward each fan's minimum.
            let mins: Vec<f64> = self.fans.iter().map(|f| f.min).collect();
            let held = self.ramp(curve, &last, &mins, dt, false);
            self.last = Some(held.clone());
            return Decision::Set(held);
        }

        self.auto_since = None;
        // A fan the curve leaves on auto while another is forced sits at its min.
        let want: Vec<f64> = wanted.iter().zip(&self.fans).map(|(w, f)| w.unwrap_or(f.min)).collect();
        let next = match &self.last {
            Some(last) => self.ramp(curve, last, &want, dt, emergency),
            // Taking over from macOS: with a ramp_up, start at each fan's minimum
            // and climb; without one (or in an emergency), go straight there.
            None if emergency || curve.ramp_up.is_none() => want,
            None => self.fans.iter().map(|f| f.min).collect(),
        };
        self.last = Some(next.clone());
        Decision::Set(next)
    }

    /// Move from `last` toward `want`, limited by the curve's ramp rates.
    /// `instant_up` skips the up limit (emergency).
    fn ramp(&self, curve: &Curve, last: &[f64], want: &[f64], dt: f64, instant_up: bool) -> Vec<f64> {
        let down = curve.ramp_down.unwrap_or(RAMP_DOWN_RPM_PER_S);
        last.iter()
            .zip(want)
            .zip(&self.fans)
            .map(|((&l, &w), f)| {
                let v = if w >= l {
                    match curve.ramp_up {
                        Some(up) if !instant_up => w.min(l + up * dt),
                        _ => w,
                    }
                } else {
                    w.max(l - down * dt)
                };
                v.clamp(f.min, f.max)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fans() -> Vec<Fan> {
        vec![Fan { min: 1500.0, max: 5500.0 }, Fan { min: 1500.0, max: 5800.0 }]
    }
    fn curve() -> Curve {
        Curve::parse("sensors = T*\npoint 60 auto\npoint 65 0%\npoint 100 max\n").unwrap()
    }

    #[test]
    fn cool_stays_auto() {
        let mut c = Controller::new(fans());
        assert_eq!(c.step(&curve(), 50.0, 0.0), Decision::Auto);
    }

    #[test]
    fn emergency_forces_max_even_if_curve_says_auto() {
        let quiet = Curve::parse("sensors = T*\npoint 60 auto\n").unwrap();
        let mut c = Controller::new(fans());
        assert_eq!(c.step(&quiet, 100.0, 0.0), Decision::Set(vec![5500.0, 5800.0]));
        assert_eq!(c.step(&quiet, f64::NAN, 1.0), Decision::Set(vec![5500.0, 5800.0]));
    }

    #[test]
    fn ramps_up_instantly_down_slowly() {
        let mut c = Controller::new(fans());
        let Decision::Set(hi) = c.step(&curve(), 100.0, 0.0) else { panic!() };
        assert_eq!(hi, vec![5500.0, 5800.0]);
        let Decision::Set(lo) = c.step(&curve(), 70.0, 2.0) else { panic!() };
        assert_eq!(lo, vec![5300.0, 5600.0]); // 2 s * 100 RPM/s
    }

    #[test]
    fn auto_only_after_hold() {
        let mut c = Controller::new(fans());
        c.step(&curve(), 70.0, 0.0);
        assert!(matches!(c.step(&curve(), 50.0, 2.0), Decision::Set(_)));
        assert!(matches!(c.step(&curve(), 50.0, 20.0), Decision::Set(_)));
        assert_eq!(c.step(&curve(), 50.0, 32.0), Decision::Auto);
        // and back up if it heats again
        assert!(matches!(c.step(&curve(), 70.0, 34.0), Decision::Set(_)));
    }

    fn slow() -> Curve {
        Curve::parse("sensors = T*\npoint 60 auto\npoint 65 0%\npoint 100 max\nramp_up = 50\nramp_down = 30\n").unwrap()
    }

    #[test]
    fn slow_ramp_starts_at_min_and_climbs() {
        let mut c = Controller::new(fans());
        assert_eq!(c.step(&slow(), 95.0, 0.0), Decision::Set(vec![1500.0, 1500.0]));
        assert_eq!(c.step(&slow(), 95.0, 2.0), Decision::Set(vec![1600.0, 1600.0]));
        assert_eq!(c.step(&slow(), 95.0, 4.0), Decision::Set(vec![1700.0, 1700.0]));
    }

    #[test]
    fn slow_ramp_down() {
        let mut c = Controller::new(fans());
        c.step(&slow(), 100.0, 0.0); // emergency: straight to max
        assert_eq!(c.step(&slow(), 66.0, 2.0), Decision::Set(vec![5440.0, 5740.0]));
    }

    #[test]
    fn emergency_skips_slow_ramp_up() {
        let mut c = Controller::new(fans());
        c.step(&slow(), 70.0, 0.0);
        assert_eq!(c.step(&slow(), 101.0, 2.0), Decision::Set(vec![5500.0, 5800.0]));
    }

    #[test]
    fn average_smooths_spikes() {
        let c = Curve::parse("sensors = T*\npoint 60 0%\npoint 100 100%\naverage = 10\n").unwrap();
        let mut ctl = Controller::new(vec![Fan { min: 0.0, max: 1000.0 }]);
        for i in 0..5 {
            ctl.step(&c, 60.0, i as f64 * 2.0);
        }
        // One 90°C spike among five 60°C readings: the curve sees 65°C.
        assert_eq!(ctl.step(&c, 90.0, 10.0), Decision::Set(vec![125.0]));
        assert_eq!(ctl.curve_temp(), Some(65.0));
        // Old samples fall out of the 10 s window.
        for i in 6..12 {
            ctl.step(&c, 90.0, i as f64 * 2.0);
        }
        assert_eq!(ctl.curve_temp(), Some(90.0));
    }

    #[test]
    fn average_never_delays_emergency() {
        let c = Curve::parse("sensors = T*\npoint 60 0%\npoint 100 100%\naverage = 60\n").unwrap();
        let mut ctl = Controller::new(vec![Fan { min: 0.0, max: 1000.0 }]);
        for i in 0..20 {
            ctl.step(&c, 50.0, i as f64 * 2.0);
        }
        assert_eq!(ctl.step(&c, 101.0, 40.0), Decision::Set(vec![1000.0]));
    }

    #[test]
    fn brief_dip_does_not_release() {
        let mut c = Controller::new(fans());
        c.step(&curve(), 70.0, 0.0);
        c.step(&curve(), 50.0, 2.0);
        c.step(&curve(), 70.0, 20.0);
        assert!(matches!(c.step(&curve(), 50.0, 40.0), Decision::Set(_)));
    }
}
