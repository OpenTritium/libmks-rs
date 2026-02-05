#![feature(maybe_uninit_uninit_array_transpose)]
#![feature(try_blocks)]
pub mod dbus;
pub mod display;
pub mod error;
pub type MksResult<T = ()> = std::result::Result<T, crate::error::MksError>;
