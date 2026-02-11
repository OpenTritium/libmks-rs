#![feature(maybe_uninit_uninit_array_transpose)]
#![feature(try_blocks)]
#![feature(const_trait_impl)]
pub mod dbus;
pub mod display;
pub mod error;
pub mod keymaps;

// Re-export core utility functions for macro hygiene
pub use dbus::utils::fetch_then_update;

pub type MksResult<T = ()> = std::result::Result<T, crate::error::MksError>;
