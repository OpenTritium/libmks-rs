#![feature(maybe_uninit_uninit_array_transpose)]
#![feature(try_blocks)]
#![feature(const_trait_impl)]
pub mod dbus;
pub mod display;
pub mod error;
pub mod keymaps;
pub type MksResult<T = ()> = std::result::Result<T, crate::error::MksError>;
