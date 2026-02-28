//! Logging helpers for module-scoped targets.
//!
//! Each module can define:
//! `const LOG_TARGET: &str = "...";`
//! and then use these macros to avoid repeating `target: LOG_TARGET`.

#[macro_export]
macro_rules! mks_trace {
    ($($key:ident $( : $capture:tt )? = $value:expr),+ ; $($arg:tt)+) => {
        ::log::trace!(target: LOG_TARGET, $($key $( : $capture )? = $value),+ ; $($arg)+)
    };
    ($($arg:tt)+) => {
        ::log::trace!(target: LOG_TARGET, $($arg)+)
    };
}

#[macro_export]
macro_rules! mks_debug {
    ($($key:ident $( : $capture:tt )? = $value:expr),+ ; $($arg:tt)+) => {
        ::log::debug!(target: LOG_TARGET, $($key $( : $capture )? = $value),+ ; $($arg)+)
    };
    ($($arg:tt)+) => {
        ::log::debug!(target: LOG_TARGET, $($arg)+)
    };
}

#[macro_export]
macro_rules! mks_info {
    ($($key:ident $( : $capture:tt )? = $value:expr),+ ; $($arg:tt)+) => {
        ::log::info!(target: LOG_TARGET, $($key $( : $capture )? = $value),+ ; $($arg)+)
    };
    ($($arg:tt)+) => {
        ::log::info!(target: LOG_TARGET, $($arg)+)
    };
}

#[macro_export]
macro_rules! mks_warn {
    ($($key:ident $( : $capture:tt )? = $value:expr),+ ; $($arg:tt)+) => {
        ::log::warn!(target: LOG_TARGET, $($key $( : $capture )? = $value),+ ; $($arg)+)
    };
    ($($arg:tt)+) => {
        ::log::warn!(target: LOG_TARGET, $($arg)+)
    };
}

#[macro_export]
macro_rules! mks_error {
    ($($key:ident $( : $capture:tt )? = $value:expr),+ ; $($arg:tt)+) => {
        ::log::error!(target: LOG_TARGET, $($key $( : $capture )? = $value),+ ; $($arg)+)
    };
    ($($arg:tt)+) => {
        ::log::error!(target: LOG_TARGET, $($arg)+)
    };
}
