use libc::{
    c_char, c_int, c_void, dl_iterate_phdr, dl_phdr_info, mprotect, size_t, PT_DYNAMIC, PT_LOAD,
};
use std::ffi::CStr;
use std::mem;
use std::ptr;
use std::slice;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use tracing::{debug, error};

use super::Patcher;
use crate::platform::Hook;

// Constants not provided by libc
const DT_NULL: i64 = 0;
const DT_PLTREL: i64 = 20;
const DT_REL: i64 = 17;
const DT_RELA: i64 = 7;
const DT_RELSZ: i64 = 18;
const DT_RELASZ: i64 = 8;
const PROT_WRITE: i32 = 0x2;
const DT_SYMTAB: i64 = 6;
const DT_STRTAB: i64 = 5;
const DT_SYMENT: i64 = 11;
const DT_STRSZ: i64 = 10;
const DT_JMPREL: i64 = 23;
const DT_PLTRELSZ: i64 = 2;

// Only these relocation types hold a plain function pointer we may overwrite.
// Patching any other type (COPY, TLS, IRELATIVE, ...) corrupts data.
#[cfg(target_arch = "x86_64")]
const ALLOWED_RELOC_TYPES: &[u32] = &[6, 7]; // R_X86_64_GLOB_DAT, R_X86_64_JUMP_SLOT
#[cfg(target_arch = "aarch64")]
const ALLOWED_RELOC_TYPES: &[u32] = &[1025, 1026]; // R_AARCH64_GLOB_DAT, R_AARCH64_JUMP_SLOT

// ELF types
#[allow(non_camel_case_types)]
#[repr(C)]
pub struct Elf64_Dyn {
    pub d_tag: i64,
    pub d_un: Elf64_Dyn_Union,
}

#[allow(non_camel_case_types)]
#[repr(C)]
pub union Elf64_Dyn_Union {
    pub d_val: i64,
    pub d_ptr: u64,
}

#[allow(non_camel_case_types)]
pub type Elf64_Addr = u64;
#[allow(non_camel_case_types)]
pub type Elf64_Xword = u64;
#[allow(non_camel_case_types)]
pub type Elf64_Sxword = i64;

#[allow(non_camel_case_types)]
#[repr(C)]
pub struct Elf64_Rel {
    pub r_offset: Elf64_Addr,
    pub r_info: Elf64_Xword,
}

#[allow(non_camel_case_types)]
#[repr(C)]
pub struct Elf64_Rela {
    pub r_offset: Elf64_Addr,
    pub r_info: Elf64_Xword,
    pub r_addend: Elf64_Sxword,
}

#[allow(non_camel_case_types)]
#[repr(C)]
pub struct Elf64_Sym {
    st_name: u32,
    st_info: u8,
    st_other: u8,
    st_shndx: u16,
    st_value: Elf64_Addr,
    st_size: u64,
}

impl Elf64_Sym {
    /// A non-PIE executable that takes an imported function's address exports
    /// it as an undefined symbol whose value is its own PLT stub. The loader
    /// then resolves the name to that stub everywhere, including for its own
    /// internal malloc, so the GOT slot behind the stub is effectively the
    /// process-wide malloc. Hooking it routes ld.so's lazy TLS allocation into
    /// our hook, whose first TLS access recurses until the stack overflows.
    fn is_canonical_plt_entry(&self) -> bool {
        const SHN_UNDEF: u16 = 0;
        self.st_shndx == SHN_UNDEF && self.st_value != 0
    }
}

struct SymbolTable {
    symtab: *const Elf64_Sym,
    strtab: *const u8,
    str_size: usize,
}

impl SymbolTable {
    unsafe fn new(dyn_info: *const Elf64_Dyn) -> Option<Self> {
        let mut symtab = ptr::null();
        let mut strtab = ptr::null();
        let mut syment: usize = 0;
        let mut str_size = 0;

        let mut cur = dyn_info;
        while (*cur).d_tag != DT_NULL {
            match (*cur).d_tag {
                tag if tag == DT_SYMTAB => symtab = (*cur).d_un.d_ptr as *const Elf64_Sym,
                tag if tag == DT_STRTAB => strtab = (*cur).d_un.d_ptr as *const u8,
                tag if tag == DT_SYMENT => syment = (*cur).d_un.d_val as usize,
                tag if tag == DT_STRSZ => str_size = (*cur).d_un.d_val as usize,
                _ => {}
            }
            cur = cur.add(1);
        }

        if symtab.is_null()
            || strtab.is_null()
            || syment != mem::size_of::<Elf64_Sym>()
            || str_size == 0
        {
            return None;
        }

        Some(SymbolTable {
            symtab,
            strtab,
            str_size,
        })
    }

    unsafe fn get_symbol_name(&self, st_name: u32) -> Option<&str> {
        if st_name == 0 || st_name as usize >= self.str_size {
            return None;
        }
        let name_ptr = self.strtab.add(st_name as usize) as *const c_char;
        let bytes = slice::from_raw_parts(name_ptr.cast(), self.str_size - st_name as usize);
        let c_str = CStr::from_bytes_until_nul(bytes).ok()?;
        c_str.to_str().ok()
    }
}

/// Linux-specific implementation of the symbol patcher
pub struct LinuxPatcher {
    hooks: Vec<Hook>,
}

/// Holds context to pass through dl_iterate_phdr's callback.
struct ElfPatcherContext<'a> {
    restore_original: bool,
    hooks: &'a [Hook],
    mappings: &'a [MemoryRegion],
}

/// A global to ensure we don't run hooking simultaneously in multiple threads.
static IS_HOOKING: AtomicBool = AtomicBool::new(false);

impl Patcher for LinuxPatcher {
    fn new() -> Self {
        LinuxPatcher { hooks: Vec::new() }
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

        let mut context = ElfPatcherContext {
            restore_original: false,
            hooks: &self.hooks,
            mappings: &read_mappings(),
        };

        debug!("Starting to apply hooks...");
        unsafe {
            dl_iterate_phdr(
                Some(phdrs_callback),
                &mut context as *mut ElfPatcherContext as *mut c_void,
            );
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

        let mut context = ElfPatcherContext {
            restore_original: true,
            hooks: &self.hooks,
            mappings: &read_mappings(),
        };

        debug!("Starting to restore original functions...");
        unsafe {
            dl_iterate_phdr(
                Some(phdrs_callback),
                &mut context as *mut ElfPatcherContext as *mut c_void,
            );
        }

        IS_HOOKING.store(false, Ordering::SeqCst);
        debug!("Finished restoring original functions");
    }
}

/// Whether `address` lies in one of the object's loaded segments.
unsafe fn object_contains(info: &dl_phdr_info, address: usize) -> bool {
    let phdrs = slice::from_raw_parts(info.dlpi_phdr, info.dlpi_phnum as usize);
    phdrs.iter().any(|phdr| {
        let start = info.dlpi_addr as usize + phdr.p_vaddr as usize;
        phdr.p_type == PT_LOAD && (start..start + phdr.p_memsz as usize).contains(&address)
    })
}

unsafe extern "C" fn phdrs_callback(
    info: *mut dl_phdr_info,
    _size: size_t,
    data: *mut c_void,
) -> c_int {
    if info.is_null() || data.is_null() {
        return 0;
    }

    let info_ref = &*info;
    let context = &mut *(data as *mut ElfPatcherContext);

    let so_name = if !info_ref.dlpi_name.is_null() {
        match CStr::from_ptr(info_ref.dlpi_name).to_str() {
            Ok(s) if !s.is_empty() => s,
            _ => "[main executable]",
        }
    } else {
        "[main executable]"
    };

    // Skip the dynamic loader, the vDSO, and this extension itself. Match file
    // names and addresses rather than path substrings, which would also skip
    // every library under a directory that happens to contain those names.
    if object_contains(info_ref, phdrs_callback as *const () as usize) {
        debug!("Skipping this extension: {}", so_name);
        return 0;
    }
    let file_name = so_name.rsplit('/').next().unwrap_or(so_name);
    if file_name.starts_with("ld-linux")
        || file_name.starts_with("ld-musl")
        || file_name.starts_with("linux-vdso.so")
    {
        debug!("Skipping library: {}", so_name);
        return 0;
    }

    // Note: libraries are intentionally re-patched on every pass. Patching is
    // idempotent, and skipping by name would permanently miss a library that
    // was dlclosed and loaded again (fresh, unpatched PLT at the same name).
    debug!("Processing library: {}", so_name);

    // Traverse the program headers to find PT_DYNAMIC
    let phdr_slice = slice::from_raw_parts(info_ref.dlpi_phdr, info_ref.dlpi_phnum as usize);
    for phdr in phdr_slice {
        if phdr.p_type == PT_DYNAMIC {
            let dyn_ptr = (info_ref.dlpi_addr + phdr.p_vaddr) as *const Elf64_Dyn;
            patch_symbols(
                dyn_ptr,
                info_ref.dlpi_addr,
                context.restore_original,
                context.hooks,
                context.mappings,
            );
        }
    }

    0
}

unsafe fn patch_symbols(
    dynamic: *const Elf64_Dyn,
    base: Elf64_Addr,
    restore_original: bool,
    hooks: &[Hook],
    mappings: &[MemoryRegion],
) {
    if dynamic.is_null() {
        debug!("Dynamic info struct is null");
        return;
    }
    let Some(symbols) = SymbolTable::new(dynamic) else {
        debug!("Could not find symbol table");
        return;
    };
    debug!("Processing dynamic section at base address: {:#x}", base);
    let patcher = RelocationPatcher {
        symbols: &symbols,
        base,
        restore_original,
        hooks,
        mappings,
    };

    // PLT relocations use whichever entry format DT_PLTREL names.
    match dynamic_value(dynamic, DT_PLTREL) {
        Some(DT_REL) => patcher.patch(dynamic_table::<Elf64_Rel>(dynamic, DT_JMPREL, DT_PLTRELSZ)),
        Some(DT_RELA) => {
            patcher.patch(dynamic_table::<Elf64_Rela>(dynamic, DT_JMPREL, DT_PLTRELSZ))
        }
        other => debug!("Unknown PLT relocation type: {:?}", other),
    }
    patcher.patch(dynamic_table::<Elf64_Rel>(dynamic, DT_REL, DT_RELSZ));
    patcher.patch(dynamic_table::<Elf64_Rela>(dynamic, DT_RELA, DT_RELASZ));
}

/// The value of a dynamic-section entry.
unsafe fn dynamic_value(dynamic: *const Elf64_Dyn, tag: i64) -> Option<i64> {
    let mut cur = dynamic;
    while (*cur).d_tag != DT_NULL {
        if (*cur).d_tag == tag {
            return Some((*cur).d_un.d_val);
        }
        cur = cur.add(1);
    }
    None
}

/// A relocation table located by an address entry and a byte-size entry.
unsafe fn dynamic_table<'a, T>(
    dynamic: *const Elf64_Dyn,
    address_tag: i64,
    size_tag: i64,
) -> &'a [T] {
    match (
        dynamic_value(dynamic, address_tag),
        dynamic_value(dynamic, size_tag),
    ) {
        (Some(address), Some(size)) if address != 0 && size > 0 => slice::from_raw_parts(
            address as usize as *const T,
            size as usize / mem::size_of::<T>(),
        ),
        _ => &[],
    }
}

trait RelocationEntry {
    fn offset(&self) -> Elf64_Addr;
    fn info(&self) -> u64;
    fn symbol_index(&self) -> u32 {
        (self.info() >> 32) as u32
    }
    fn reloc_type(&self) -> u32 {
        (self.info() & 0xffff_ffff) as u32
    }
}

impl RelocationEntry for Elf64_Rel {
    fn offset(&self) -> Elf64_Addr {
        self.r_offset
    }

    fn info(&self) -> u64 {
        self.r_info
    }
}

impl RelocationEntry for Elf64_Rela {
    fn offset(&self) -> Elf64_Addr {
        self.r_offset
    }

    fn info(&self) -> u64 {
        self.r_info
    }
}

/// Rewrites the hooked symbols' slots named by one object's relocations.
struct RelocationPatcher<'a> {
    symbols: &'a SymbolTable,
    base: Elf64_Addr,
    restore_original: bool,
    hooks: &'a [Hook],
    mappings: &'a [MemoryRegion],
}

impl RelocationPatcher<'_> {
    unsafe fn patch<T: RelocationEntry>(&self, relocations: &[T]) {
        for reloc in relocations {
            if !ALLOWED_RELOC_TYPES.contains(&reloc.reloc_type()) {
                continue;
            }
            let symbol = &*self.symbols.symtab.add(reloc.symbol_index() as usize);
            if symbol.is_canonical_plt_entry() {
                continue;
            }
            let Some(name) = self.symbols.get_symbol_name(symbol.st_name) else {
                continue;
            };
            for hook in self.hooks.iter().filter(|hook| hook.symbol_name == name) {
                let target = if self.restore_original {
                    hook.original_fn
                } else {
                    hook.intercept_fn
                };
                patch_symbol(
                    (self.base + reloc.offset()) as *mut c_void,
                    target,
                    hook,
                    self.restore_original,
                    self.mappings,
                );
            }
        }
    }
}

#[derive(Debug)]
struct MemoryRegion {
    start: usize,
    end: usize,
    protection: i32,
}

fn read_mappings() -> Vec<MemoryRegion> {
    // A snapshot avoids doing file I/O for every relocation. If procfs is
    // unavailable, skip native patching rather than weaken unknown protections.
    std::fs::read_to_string("/proc/self/maps")
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let (start, end) = fields.next()?.split_once('-')?;
            let permissions = fields.next()?.as_bytes();
            if permissions.len() < 3 {
                return None;
            }
            let mut protection = 0;
            if permissions[0] == b'r' {
                protection |= libc::PROT_READ;
            }
            if permissions[1] == b'w' {
                protection |= libc::PROT_WRITE;
            }
            if permissions[2] == b'x' {
                protection |= libc::PROT_EXEC;
            }
            Some(MemoryRegion {
                start: usize::from_str_radix(start, 16).ok()?,
                end: usize::from_str_radix(end, 16).ok()?,
                protection,
            })
        })
        .collect()
}

unsafe fn patch_symbol(
    symbol_addr: *mut c_void,
    new_fn: *mut c_void,
    hook: &Hook,
    restore_original: bool,
    mappings: &[MemoryRegion],
) {
    let address = symbol_addr as usize;
    if !address.is_multiple_of(mem::align_of::<AtomicPtr<c_void>>()) {
        return;
    }
    let slot = &*symbol_addr.cast::<AtomicPtr<c_void>>();
    let current = slot.load(Ordering::Acquire);
    debug!(symbol = hook.symbol_name, ?current, original = ?hook.original_fn, ?new_fn, "Inspecting native binding");
    if current == new_fn || (restore_original && current != hook.intercept_fn) {
        return;
    }
    // Do not replace an allocator with a different implementation. Calling
    // our global original for a custom allocator would corrupt its heap.
    if !restore_original && current != hook.original_fn {
        return;
    }
    let Some(region) = mappings
        .iter()
        .find(|m| address >= m.start && address + mem::size_of::<usize>() <= m.end)
    else {
        return;
    };
    if region.protection & libc::PROT_EXEC != 0 {
        return;
    }
    let page_size = libc::sysconf(libc::_SC_PAGESIZE);
    if page_size <= 0 {
        return;
    }
    let page_size = page_size as usize;
    let page = (address & !(page_size - 1)) as *mut c_void;
    let writable = region.protection & PROT_WRITE != 0;
    if !writable && mprotect(page, page_size, region.protection | PROT_WRITE) != 0 {
        return;
    }
    // Aligned atomic replacement avoids a Rust data race with other hookers.
    let _ = slot.compare_exchange(current, new_fn, Ordering::AcqRel, Ordering::Acquire);
    if !writable && mprotect(page, page_size, region.protection) != 0 {
        error!("Could not restore original GOT page protection");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symbol(st_shndx: u16, st_value: Elf64_Addr) -> Elf64_Sym {
        Elf64_Sym {
            st_name: 1,
            st_info: 0x12, // STB_GLOBAL, STT_FUNC
            st_other: 0,
            st_shndx,
            st_value,
            st_size: 0,
        }
    }

    #[test]
    fn only_undefined_symbols_with_a_value_are_canonical_plt_entries() {
        // Non-PIE executable that takes malloc's address: never patch.
        assert!(symbol(0, 0x127e550).is_canonical_plt_entry());
        // Ordinary import in a PIE executable or shared library: patch.
        assert!(!symbol(0, 0).is_canonical_plt_entry());
        // Defined symbol (a custom allocator): not an import at all.
        assert!(!symbol(14, 0x4000).is_canonical_plt_entry());
    }
}
