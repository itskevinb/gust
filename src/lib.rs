pub mod control;
pub mod curve;
pub mod fanctl;
pub mod sensors;
pub mod smc;

/// Where gustd publishes its state for `gust status` and future UIs.
pub const STATUS_PATH: &str = "/var/run/gust.json";
