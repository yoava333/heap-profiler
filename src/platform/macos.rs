use libc::{
    c_char, c_int, c_void, dlclose, dlopen, dlsym, intptr_t, kern_return_t, mach_port_t, size_t,
    vm_address_t, vm_prot_t,
};
use std::collections::HashSet;
use std::ffi::CStr;
use std::mem;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{debug, error, warn};

use super::Patcher;
use crate::platform::Hook;

/// macOS-specific implementation of the symbol patcher
pub struct MacOSPatcher {
    hooks: Vec<Hook>,
    patched_symbols: HashSet<String>,
    #[allow(unused)]
    self_so_name: String,
}

/// A global to ensure we don't run hooking simultaneously in multiple threads.
static IS_HOOKING: AtomicBool = AtomicBool::new(false);

extern "C" {
    fn mach_task_self() -> mach_port_t;
    fn vm_protect(
        task: mach_port_t,
        address: vm_address_t,
        size: size_t,
        set_maximum: c_int,
        new_protection: vm_prot_t,
    ) -> kern_return_t;
    fn _dyld_image_count() -> u32;
    fn _dyld_get_image_header(index: u32) -> *const MachHeader;
    fn _dyld_get_image_vmaddr_slide(index: u32) -> intptr_t;
    fn _dyld_get_image_name(index: u32) -> *const c_char;
}

#[repr(C)]
struct MachHeader {
    pub magic: u32,
    pub cputype: i32,
    pub cpusubtype: i32,
    pub filetype: u32,
    pub ncmds: u32,
    pub sizeofcmds: u32,
    pub flags: u32,
    pub reserved: u32,
}

#[repr(C)]
struct SegmentCommand64 {
    cmd: u32,
    cmdsize: u32,
    segname: [u8; 16],
    vmaddr: u64,
    vmsize: u64,
    fileoff: u64,
    filesize: u64,
    maxprot: i32,
    initprot: i32,
    nsects: u32,
    flags: u32,
}

#[repr(C)]
struct Section64 {
    sectname: [u8; 16],
    segname: [u8; 16],
    addr: u64,
    size: u64,
    offset: u32,
    align: u32,
    reloff: u32,
    nreloc: u32,
    flags: u32,
    reserved1: u32,
    reserved2: u32,
    reserved3: u32,
}

const SECTION_TYPE_MASK: u32 = 0x000000ff;
const S_LAZY_SYMBOL_POINTERS: u32 = 0x7;
const S_NON_LAZY_SYMBOL_POINTERS: u32 = 0x6;

impl Patcher for MacOSPatcher {
    fn new(self_name: String) -> Self {
        MacOSPatcher {
            hooks: Vec::new(),
            patched_symbols: HashSet::new(),
            self_so_name: self_name,
        }
    }

    fn register_hook(&mut self, hook: Hook) {
        self.hooks.push(hook);
    }

    fn apply_hooks(&mut self) {
        if IS_HOOKING
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            error!("Another hooking operation is already in progress!");
            return;
        }

        debug!("Starting to apply hooks...");
        unsafe {
            patch_symbols_in_all_shared_objects(&mut self.patched_symbols, &self.hooks, false);
        }
        IS_HOOKING.store(false, Ordering::SeqCst);
        debug!("Finished applying hooks");
    }

    fn restore_hooks(&mut self) {
        if IS_HOOKING
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            error!("Another hooking operation is already in progress!");
            return;
        }

        debug!("Starting to restore original functions...");
        unsafe {
            patch_symbols_in_all_shared_objects(&mut self.patched_symbols, &self.hooks, true);
        }
        self.patched_symbols.clear();
        IS_HOOKING.store(false, Ordering::SeqCst);
        debug!("Finished restoring original functions");
    }
}

unsafe fn make_page_writable(address: usize, size: usize) -> Result<(), &'static str> {
    let page_size = libc::sysconf(libc::_SC_PAGESIZE) as usize;
    let page_mask = !(page_size - 1);
    let page_address = address & page_mask;

    let end_address = address + size;
    let end_page = (end_address + page_size - 1) & page_mask;
    let total_size = end_page - page_address;

    debug!("Attempting to make memory writable:");
    debug!("  Original address: 0x{:x}", address);
    debug!("  Page address: 0x{:x}", page_address);
    debug!("  Size: {} bytes", total_size);

    let new_protection = libc::PROT_READ | libc::PROT_WRITE;
    let result = vm_protect(
        mach_task_self(),
        page_address as vm_address_t,
        total_size as size_t,
        0,
        new_protection as vm_prot_t,
    );

    if result == 0 {
        debug!("Successfully made memory writable");
        Ok(())
    } else {
        warn!("vm_protect failed, trying mmap");

        let map = libc::mmap(
            page_address as *mut c_void,
            total_size,
            new_protection,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED,
            -1,
            0,
        );

        if map != libc::MAP_FAILED {
            debug!("Successfully created new writable mapping");
            Ok(())
        } else {
            error!("Failed to make memory writable");
            Err("Failed to make memory writable")
        }
    }
}

unsafe fn patch_symbol_addr(hook: &Hook, addr: *mut *mut c_void, restore_original: bool) -> bool {
    debug!("Attempting to patch symbol at address: {:p}", addr);

    if make_page_writable(addr as usize, mem::size_of::<*mut c_void>()).is_err() {
        warn!("Failed to make memory writable at {:p}", addr);

        let mut buf = [0u8; 32];
        let trampoline = libc::mmap(
            ptr::null_mut(),
            buf.len(),
            libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        );

        if trampoline != libc::MAP_FAILED {
            debug!("Created trampoline at {:p}", trampoline);

            buf[0] = 0xff;
            buf[1] = 0x25;
            buf[2..6].copy_from_slice(&[0, 0, 0, 0]);

            let target = if restore_original {
                hook.original_fn
            } else {
                hook.intercept_fn
            };

            let target_addr = target as usize;
            buf[6..14].copy_from_slice(&target_addr.to_ne_bytes());

            ptr::copy_nonoverlapping(buf.as_ptr(), trampoline as *mut u8, buf.len());

            if make_page_writable(addr as usize, mem::size_of::<*mut c_void>()).is_ok() {
                ptr::write(addr, trampoline);
                true
            } else {
                error!("Failed to make trampoline memory writable");
                libc::munmap(trampoline, buf.len());
                false
            }
        } else {
            error!("Failed to create trampoline");
            false
        }
    } else {
        if restore_original {
            debug!(
                "Restoring original function at {:p} with {:p}",
                addr, hook.original_fn
            );
            ptr::write(addr, hook.original_fn);
        } else {
            debug!(
                "Writing intercept function at {:p} with {:p}",
                addr, hook.intercept_fn as *mut c_void
            );
            ptr::write(addr, hook.intercept_fn as *mut c_void);
        }
        true
    }
}

unsafe fn get_symbol_name(addr: *const c_void) -> Option<String> {
    let mut info: libc::Dl_info = mem::zeroed();
    if libc::dladdr(addr, &mut info) != 0 && !info.dli_sname.is_null() {
        Some(
            CStr::from_ptr(info.dli_sname)
                .to_string_lossy()
                .into_owned(),
        )
    } else {
        None
    }
}

unsafe fn patch_symbols_in_section(
    section: &Section64,
    slide: u64,
    _image_name: &CStr,
    hooks: &[Hook],
    restore_original: bool,
) {
    debug!("Examining section at address: 0x{:x}", section.addr);

    let symbol_addr_table = (slide + section.addr) as *mut *mut c_void;
    let count = (section.size as usize) / mem::size_of::<*mut c_void>();

    debug!("Found {} potential symbol pointers", count);

    for i in 0..count {
        let symbol_ptr = symbol_addr_table.add(i);
        let current_fn = *symbol_ptr;

        if let Some(symbol_name) = get_symbol_name(current_fn) {
            debug!("Found symbol: {} at {:p}", symbol_name, current_fn);

            for hook in hooks {
                if symbol_name.ends_with(hook.symbol_name) {
                    debug!("Found target symbol {} to patch!", hook.symbol_name);
                    patch_symbol_addr(hook, symbol_ptr, restore_original);
                }
            }
        }
    }
}

unsafe fn patch_stubs(
    section: &Section64,
    slide: u64,
    _image_name: &CStr,
    element_size: usize,
    hooks: &[Hook],
    restore_original: bool,
) {
    if element_size == 0 {
        return;
    }

    debug!("Examining stubs section at address: 0x{:x}", section.addr);

    let stub_base = (slide + section.addr) as u64;
    let count = (section.size as usize) / element_size;

    debug!("Found {} potential stubs", count);

    for i in 0..count {
        let stub_addr = stub_base + (i * element_size) as u64;
        if let Some(symbol_addr) = lazy_pointer_from_stub(stub_addr) {
            if let Some(symbol_name) = get_symbol_name(symbol_addr) {
                debug!(
                    "Found stub for symbol: {} at 0x{:x}",
                    symbol_name, stub_addr
                );

                for hook in hooks {
                    if symbol_name.ends_with(hook.symbol_name) {
                        debug!("Found target symbol {} to patch in stub!", hook.symbol_name);
                        patch_symbol_addr(hook, symbol_addr as *mut *mut c_void, restore_original);
                    }
                }
            }
        }
    }
}

unsafe fn lazy_pointer_from_stub(_stub_addr: u64) -> Option<*mut c_void> {
    None
}

unsafe fn patch_symbols_in_shared_object(
    header: *const MachHeader,
    slide: intptr_t,
    image_name: &CStr,
    patched: &mut HashSet<String>,
    hooks: &[Hook],
    restore_original: bool,
) {
    let image_str = image_name.to_string_lossy().to_string();
    if !restore_original && patched.contains(&*image_str) {
        return;
    }
    if !restore_original {
        patched.insert(image_str.clone());
    }

    if image_str.contains("libdyld.dylib") || image_str.contains("/usr/lib/system/") {
        return;
    }

    let ncmds = (*header).ncmds;
    let first_cmd =
        (header as *const u8).add(mem::size_of::<MachHeader>()) as *const SegmentCommand64;
    let mut cmd_ptr = first_cmd;

    for _ in 0..ncmds {
        let segment = &*cmd_ptr;
        if segment.cmd == 0x19 {
            let segname = segment.segname;
            let segname_str = std::str::from_utf8(&segname)
                .unwrap_or_default()
                .trim_end_matches('\0');

            if segname_str == "__DATA"
                || segname_str == "__DATA_CONST"
                || segname_str == "__AUTH_CONST"
                || segname_str == "__TEXT"
            {
                let section_ptr = (cmd_ptr as *const u8).add(mem::size_of::<SegmentCommand64>())
                    as *const Section64;
                for i in 0..segment.nsects {
                    let sect = &*section_ptr.add(i as usize);
                    let sectname = std::str::from_utf8(&sect.sectname)
                        .unwrap_or_default()
                        .trim_end_matches('\0');
                    let section_type = sect.flags & SECTION_TYPE_MASK;

                    if dyld_shared_cache_contains_path(image_name)
                        && (sectname == "__auth_stubs" || sectname == "__stubs")
                    {
                        patch_stubs(
                            sect,
                            slide as u64,
                            image_name,
                            sect.reserved2 as usize,
                            hooks,
                            restore_original,
                        );
                    } else if section_type == S_LAZY_SYMBOL_POINTERS
                        || section_type == S_NON_LAZY_SYMBOL_POINTERS
                    {
                        patch_symbols_in_section(
                            sect,
                            slide as u64,
                            image_name,
                            hooks,
                            restore_original,
                        );
                    }
                }
            }
        }
        cmd_ptr = (cmd_ptr as *const u8).add(segment.cmdsize as usize) as *const SegmentCommand64;
    }
}

fn dyld_shared_cache_contains_path(image_name: &CStr) -> bool {
    static mut FN_PTR: Option<unsafe extern "C" fn(*const c_char) -> bool> = None;
    static INIT: std::sync::Once = std::sync::Once::new();

    unsafe {
        INIT.call_once(|| {
            let handle = dlopen(ptr::null(), 1);
            if !handle.is_null() {
                let sym = dlsym(
                    handle,
                    b"_dyld_shared_cache_contains_path\0".as_ptr() as *const _,
                );
                if !sym.is_null() {
                    FN_PTR = Some(mem::transmute(sym));
                }
                dlclose(handle);
            }
        });

        if let Some(fp) = FN_PTR {
            fp(image_name.as_ptr())
        } else {
            false
        }
    }
}

unsafe fn patch_symbols_in_all_shared_objects(
    patched: &mut HashSet<String>,
    hooks: &[Hook],
    restore_original: bool,
) {
    let count = _dyld_image_count();
    for i in 0..count {
        let header = _dyld_get_image_header(i);
        if header.is_null() {
            continue;
        }
        let slide = _dyld_get_image_vmaddr_slide(i);
        let name_ptr = _dyld_get_image_name(i);
        if name_ptr.is_null() {
            continue;
        }
        let image_name = CStr::from_ptr(name_ptr);
        if image_name.to_string_lossy().contains("libdyld.dylib")
            || image_name.to_string_lossy().contains("/usr/lib/system/")
        {
            continue;
        }
        patch_symbols_in_shared_object(header, slide, image_name, patched, hooks, restore_original);
    }
}
