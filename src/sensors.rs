//! Find and read the temperature sensors a curve cares about.

use crate::curve::Curve;
use crate::smc::{Error, Smc};

/// Plausible range for a real reading. Outside it the sensor is ignored.
pub fn plausible(c: f64) -> bool {
    (1.0..=130.0).contains(&c)
}

/// SMC keys matching the curve's `sensors =` patterns. Enumerating every key
/// is slow-ish (~0.1 s), so callers cache this and refresh on curve reload.
pub fn matching_keys(smc: &Smc, curve: &Curve) -> Result<Vec<String>, Error> {
    Ok(smc.keys()?.into_iter().filter(|k| curve.matches(k)).collect())
}

/// Hottest plausible reading among `keys`, with how many read OK.
pub fn hottest(smc: &Smc, keys: &[String]) -> Option<(String, f64, usize)> {
    let mut best: Option<(String, f64)> = None;
    let mut ok = 0;
    for k in keys {
        let Some(c) = smc.read(k).ok().and_then(|v| v.as_f64()) else { continue };
        if !plausible(c) {
            continue;
        }
        ok += 1;
        if best.as_ref().is_none_or(|b| c > b.1) {
            best = Some((k.clone(), c));
        }
    }
    best.map(|(k, c)| (k, c, ok))
}
