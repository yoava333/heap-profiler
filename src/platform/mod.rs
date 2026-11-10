#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::MacOSPatcher as PlatformPatcher;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::LinuxPatcher as PlatformPatcher;

/// Logged when a patcher skips this extension; tests match this text.
pub(crate) const SKIPPING_THIS_EXTENSION: &str = "Skipping this extension";

#[derive(Debug, Clone, Copy)]
pub struct Hook {
    pub symbol_name: &'static str,
    pub original_fn: *mut libc::c_void,
    pub intercept_fn: *mut libc::c_void,
}

// Make Hook explicitly Send + Sync since we're guaranteeing thread-safety
unsafe impl Send for Hook {}
unsafe impl Sync for Hook {}

/// Common trait that must be implemented by platform-specific patchers
pub trait Patcher {
    fn new() -> Self;
    fn register_hook(&mut self, hook: Hook);
    fn apply_hooks(&mut self);
    fn restore_hooks(&mut self);
}
