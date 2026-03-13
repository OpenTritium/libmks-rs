#![feature(likely_unlikely)]
pub mod dbus;
pub mod display;
pub mod error;
pub mod keymaps;
pub mod log;

pub type MksResult<T = ()> = std::result::Result<T, error::MksError>;
