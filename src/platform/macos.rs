use libc::{
    c_char, c_int, c_void, intptr_t, kern_return_t, mach_port_t, size_t, vm_address_t, vm_prot_t,
};
use std::ffi::CStr;
use std::mem;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use tracing::{debug, error, warn};

use super::Patcher;
use crate::platform::Hook;

/// macOS-specific implementation of the symbol patcher
pub struct MacOSPatcher {
    hooks: Vec<Hook>,
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
    fn mach_vm_region(
        task: mach_port_t,
        address: *mut u64,
        size: *mut u64,
        flavor: c_int,
        info: *mut c_int,
        count: *mut u32,
        object: *mut mach_port_t,
    ) -> kern_return_t;
    fn mach_port_deallocate(task: mach_port_t, name: mach_port_t) -> kern_return_t;
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
            patch_symbols_in_all_shared_objects(&self.hooks, false);
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
            patch_symbols_in_all_shared_objects(&self.hooks, true);
        }
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
        // Do NOT fall back to mmap(MAP_FIXED) here: an anonymous fixed mapping
        // replaces the whole page with zeros, destroying every other symbol
        // pointer on it. Skipping the symbol is always safer.
        error!("vm_protect failed; cannot make memory writable");
        Err("Failed to make memory writable")
    }
}

unsafe fn patch_symbol_addr(hook: &Hook, addr: *mut *mut c_void, restore_original: bool) -> bool {
    if !(addr as usize).is_multiple_of(mem::align_of::<AtomicPtr<c_void>>()) {
        return false;
    }
    let slot = &*addr.cast::<AtomicPtr<c_void>>();
    let current = slot.load(Ordering::Acquire);
    let expected = if restore_original {
        hook.intercept_fn
    } else {
        hook.original_fn
    };
    if current != expected {
        return false;
    }
    debug!(symbol = hook.symbol_name, "Patching native slot");
    // Query current protection: dyld may have sealed __DATA_CONST read-only
    // even when the Mach-O segment's initprot says writable.
    let mut region_address = addr as u64;
    let mut region_size = 0;
    let mut info = [0i32; 32];
    let mut count = 9;
    let mut object = 0;
    let task = mach_task_self();
    if mach_vm_region(
        task,
        &mut region_address,
        &mut region_size,
        9,
        info.as_mut_ptr(),
        &mut count,
        &mut object,
    ) != 0
    {
        return false;
    }
    if object != 0 {
        mach_port_deallocate(task, object);
    }
    if region_address > addr as u64 {
        return false;
    }
    let protection = info[0];
    if protection & libc::PROT_EXEC != 0 {
        return false;
    }
    let page_size = libc::sysconf(libc::_SC_PAGESIZE);
    if page_size <= 0 {
        return false;
    }
    let page_size = page_size as usize;
    let page = addr as usize & !(page_size - 1);
    let writable = protection & libc::PROT_WRITE != 0;
    if !writable && make_page_writable(addr as usize, mem::size_of::<*mut c_void>()).is_err() {
        return false;
    }
    let target = if restore_original {
        hook.original_fn
    } else {
        hook.intercept_fn
    };
    let changed = slot
        .compare_exchange(expected, target, Ordering::AcqRel, Ordering::Acquire)
        .is_ok();
    if !writable && vm_protect(task, page as vm_address_t, page_size, 0, protection) != 0 {
        warn!("Failed to restore symbol page protection");
    }
    changed
}

unsafe fn patch_symbols_in_section(
    section: &Section64,
    slide: u64,
    _image_name: &CStr,
    hooks: &[Hook],
    restore_original: bool,
) {
    debug!("Examining section at address: 0x{:x}", section.addr);

    let symbol_addr_table = slide.wrapping_add(section.addr) as *mut *mut c_void;
    let count = (section.size as usize) / mem::size_of::<*mut c_void>();

    debug!("Found {} potential symbol pointers", count);

    for i in 0..count {
        let symbol_ptr = symbol_addr_table.add(i);
        for hook in hooks {
            patch_symbol_addr(hook, symbol_ptr, restore_original);
        }
    }
}

unsafe fn patch_symbols_in_shared_object(
    header: *const MachHeader,
    slide: intptr_t,
    image_name: &CStr,
    hooks: &[Hook],
    restore_original: bool,
) {
    // NOTE: images inside the dyld shared cache use chained fixups / __stubs
    // sections that this patcher does not support; native allocations made BY
    // system libraries are therefore not intercepted on macOS. Python-level
    // allocations are always tracked via the PyMem allocator hooks.
    //
    // Note: images are intentionally re-patched on every pass. Patching is
    // idempotent, and skipping by name would permanently miss an image that
    // was unloaded and loaded again at the same path.
    let image_str = image_name.to_string_lossy();
    if image_str.contains("libdyld.dylib")
        || image_str.contains("/usr/lib/system/")
        || image_str.contains("heap_profiler")
    {
        return;
    }

    if (*header).magic != 0xfeedfacf || (*header).flags & 0x80000000 != 0 {
        return;
    }
    let ncmds = (*header).ncmds;
    let commands = (header as *const u8).add(mem::size_of::<MachHeader>());
    let end = commands.add((*header).sizeofcmds as usize) as usize;
    let mut cursor = commands;
    for _ in 0..ncmds {
        if cursor as usize + 8 > end {
            break;
        }
        let cmd = ptr::read_unaligned(cursor.cast::<u32>());
        let cmdsize = ptr::read_unaligned(cursor.add(4).cast::<u32>()) as usize;
        if cmdsize < 8 || cmdsize > end - cursor as usize {
            break;
        }
        if cmd == 0x19 && cmdsize >= mem::size_of::<SegmentCommand64>() {
            let segment = &*cursor.cast::<SegmentCommand64>();
            // __AUTH* entries require pointer authentication we cannot supply.
            if !segment.segname.starts_with(b"__AUTH") {
                let section_bytes = cmdsize - mem::size_of::<SegmentCommand64>();
                if segment.nsects as usize <= section_bytes / mem::size_of::<Section64>() {
                    let sections = cursor
                        .add(mem::size_of::<SegmentCommand64>())
                        .cast::<Section64>();
                    for i in 0..segment.nsects as usize {
                        let section = &*sections.add(i);
                        let kind = section.flags & SECTION_TYPE_MASK;
                        if kind == S_LAZY_SYMBOL_POINTERS || kind == S_NON_LAZY_SYMBOL_POINTERS {
                            patch_symbols_in_section(
                                section,
                                slide as u64,
                                image_name,
                                hooks,
                                restore_original,
                            );
                        }
                    }
                }
            }
        }
        cursor = cursor.add(cmdsize);
    }
}

unsafe fn patch_symbols_in_all_shared_objects(hooks: &[Hook], restore_original: bool) {
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
            || image_name.to_string_lossy().contains("heap_profiler")
        {
            continue;
        }
        patch_symbols_in_shared_object(header, slide, image_name, hooks, restore_original);
    }
}
