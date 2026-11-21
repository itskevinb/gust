//! The only code that writes to the SMC.
//!
//! Rules enforced here, not by callers:
//! - only F{n}Md, F{n}Tg (n < FNum) and Ftst are ever written;
//! - every target is clamped to that fan's own F{n}Mn..F{n}Mx;
//! - `release` hands every fan back to macOS, and Drop calls it.
//!
//! Write sequence for Apple Silicon follows exelban/stats (MIT), SMC/smc.swift:
//! try F{n}Md=1 directly; if refused and Ftst exists, set Ftst=1, wait ~3 s
//! for thermalmonitord to yield, retry Md. Release = Md=0, Tg=0, Ftst=0.

use crate::smc::{Error, Smc};
use std::thread::sleep;
use std::time::Duration;

#[derive(Clone, Copy, Debug)]
pub struct Fan {
    pub min: f64,
    pub max: f64,
}

pub struct FanControl {
    smc: Smc,
    fans: Vec<Fan>,
    has_ftst: bool,
    forced: Vec<bool>,
}

fn num(smc: &Smc, key: &str) -> Result<f64, Error> {
    smc.read(key)?.as_f64().ok_or(Error::Smc { key: key.into(), code: 0xff })
}

/// Read fan count and ranges. Read-only; usable without root.
pub fn read_fans(smc: &Smc) -> Result<Vec<Fan>, Error> {
    let n = num(smc, "FNum")? as usize;
    (0..n)
        .map(|i| Ok(Fan { min: num(smc, &format!("F{i}Mn"))?, max: num(smc, &format!("F{i}Mx"))? }))
        .collect()
}

impl FanControl {
    pub fn open() -> Result<Self, Error> {
        let smc = Smc::open()?;
        let fans = read_fans(&smc)?;
        // gust writes F{n}Tg as a little-endian f32 and F{n}Md as one byte,
        // which is what Apple Silicon uses. Intel Macs use other types (fpe2,
        // FS! bitmask); refuse rather than write the wrong bytes.
        for i in 0..fans.len() {
            for (key, kind, size) in [(format!("F{i}Tg"), "flt ", 4), (format!("F{i}Md"), "ui8 ", 1)] {
                let v = smc.read(&key)?;
                if v.kind != kind || v.bytes.len() != size {
                    return Err(Error::Unsupported(format!(
                        "{key} is [{}] {} bytes, gust only knows [{kind}] {size} bytes (Apple Silicon)",
                        v.kind,
                        v.bytes.len()
                    )));
                }
            }
        }
        let has_ftst = smc.read("Ftst").is_ok();
        let forced = vec![false; fans.len()];
        Ok(FanControl { smc, fans, has_ftst, forced })
    }

    pub fn fans(&self) -> &[Fan] {
        &self.fans
    }

    fn allowed(&self, key: &str) -> bool {
        if key == "Ftst" {
            return self.has_ftst;
        }
        let b = key.as_bytes();
        b.len() == 4
            && b[0] == b'F'
            && b[1].is_ascii_digit()
            && ((b[1] - b'0') as usize) < self.fans.len()
            && (&key[2..] == "Md" || &key[2..] == "Tg")
    }

    fn write(&self, key: &str, bytes: &[u8]) -> Result<(), Error> {
        assert!(self.allowed(key), "gust refuses to write SMC key {key:?}");
        self.smc.write(key, bytes)
    }

    fn write_retry(&self, key: &str, bytes: &[u8], tries: u32, gap: Duration) -> Result<(), Error> {
        let mut last = Ok(());
        for _ in 0..tries {
            last = self.write(key, bytes);
            if last.is_ok() {
                return last;
            }
            sleep(gap);
        }
        last
    }

    /// Current F{n}Md as read from the SMC (1 = forced).
    pub fn mode(&self, i: usize) -> Option<u8> {
        self.smc.read(&format!("F{i}Md")).ok()?.bytes.first().copied()
    }

    fn force(&mut self, i: usize) -> Result<(), Error> {
        let key = format!("F{i}Md");
        if self.write(&key, &[1]).is_ok() {
            self.forced[i] = true;
            return Ok(());
        }
        if !self.has_ftst {
            return self.write_retry(&key, &[1], 10, Duration::from_millis(50)).map(|_| self.forced[i] = true);
        }
        self.write_retry("Ftst", &[1], 100, Duration::from_millis(50))?;
        sleep(Duration::from_secs(3));
        self.write_retry(&key, &[1], 300, Duration::from_millis(100))?;
        self.forced[i] = true;
        Ok(())
    }

    /// Set fan `i` to `rpm`, clamped to its range. Forces manual mode if needed.
    /// Returns the RPM actually written.
    pub fn set_rpm(&mut self, i: usize, rpm: f64) -> Result<f64, Error> {
        let fan = self.fans[i];
        let rpm = if rpm.is_finite() { rpm.clamp(fan.min, fan.max) } else { fan.max };
        if !self.forced[i] || self.mode(i) != Some(1) {
            self.force(i)?;
        }
        self.write_retry(&format!("F{i}Tg"), &(rpm as f32).to_le_bytes(), 10, Duration::from_millis(50))?;
        Ok(rpm)
    }

    /// Hand every fan back to macOS. Tries every step even if one fails.
    pub fn release(&mut self) -> Result<(), Error> {
        let mut first_err = None;
        for i in 0..self.fans.len() {
            for (key, bytes) in [(format!("F{i}Md"), vec![0u8]), (format!("F{i}Tg"), 0f32.to_le_bytes().to_vec())] {
                if let Err(e) = self.write_retry(&key, &bytes, 10, Duration::from_millis(50)) {
                    first_err.get_or_insert(e);
                }
            }
            self.forced[i] = false;
        }
        if self.has_ftst
            && let Err(e) = self.write_retry("Ftst", &[0], 10, Duration::from_millis(50)) {
                first_err.get_or_insert(e);
            }
        first_err.map_or(Ok(()), Err)
    }
}

impl Drop for FanControl {
    fn drop(&mut self) {
        if let Err(e) = self.release() {
            eprintln!("gustd: release on drop failed: {e}");
        }
    }
}

/// Standalone release on a fresh SMC connection: used at startup (in case a
/// previous run was killed) and from the panic hook.
pub fn emergency_release() -> Result<(), Error> {
    // Drop releases a second time; that's harmless.
    FanControl::open()?.release()
}
