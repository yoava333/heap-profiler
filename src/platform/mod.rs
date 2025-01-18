#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::MacOSPatcher as PlatformPatcher;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::LinuxPatcher as PlatformPatcher;

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
    fn new(self_name: String) -> Self;
    fn register_hook(&mut self, hook: Hook);
    fn apply_hooks(&mut self);
    #[allow(dead_code)]
    fn restore_hooks(&mut self);
}
