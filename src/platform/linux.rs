use libc::{c_char, c_int, c_void, dl_iterate_phdr, dl_phdr_info, mprotect, size_t, PT_DYNAMIC};
use std::ffi::CStr;
use std::mem;
use std::ptr;
use std::slice;
use std::sync::atomic::{AtomicBool, Ordering};
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
const PROT_READ: i32 = 0x1;
const PROT_WRITE: i32 = 0x2;
const DT_SYMTAB: i64 = 6;
const DT_STRTAB: i64 = 5;
const DT_SYMENT: i64 = 11;
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

struct SymbolTable {
    symtab: *const Elf64_Sym,
    strtab: *const u8,
    sym_count: usize,
}

impl SymbolTable {
    unsafe fn new(dyn_info: *const Elf64_Dyn) -> Option<Self> {
        let mut symtab = ptr::null();
        let mut strtab = ptr::null();
        let mut syment: usize = 0;

        let mut cur = dyn_info;
        while (*cur).d_tag != DT_NULL {
            match (*cur).d_tag {
                tag if tag == DT_SYMTAB => symtab = (*cur).d_un.d_ptr as *const Elf64_Sym,
                tag if tag == DT_STRTAB => strtab = (*cur).d_un.d_ptr as *const u8,
                tag if tag == DT_SYMENT => syment = (*cur).d_un.d_val as usize,
                _ => {}
            }
            cur = cur.add(1);
        }

        if symtab.is_null() || strtab.is_null() || syment == 0 {
            return None;
        }

        // Calculate symbol count (approximate)
        let sym_count = ((strtab as usize - symtab as usize) / syment) as usize;

        Some(SymbolTable {
            symtab,
            strtab,
            sym_count,
        })
    }

    unsafe fn get_symbol_name(&self, st_name: u32) -> Option<&str> {
        if st_name == 0 {
            return None;
        }
        let name_ptr = self.strtab.add(st_name as usize) as *const c_char;
        let c_str = CStr::from_ptr(name_ptr);
        c_str.to_str().ok()
    }
}

/// Linux-specific implementation of the symbol patcher
pub struct LinuxPatcher {
    hooks: Vec<Hook>,
    self_so_name: String,
}

/// Holds context to pass through dl_iterate_phdr's callback.
struct ElfPatcherContext<'a> {
    restore_original: bool,
    self_so_name: &'a str,
    hooks: &'a [Hook],
}

/// A global to ensure we don't run hooking simultaneously in multiple threads.
static IS_HOOKING: AtomicBool = AtomicBool::new(false);

impl Patcher for LinuxPatcher {
    fn new(self_name: String) -> Self {
        LinuxPatcher {
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

        let mut context = ElfPatcherContext {
            restore_original: false,
            self_so_name: &self.self_so_name,
            hooks: &self.hooks,
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
            self_so_name: &self.self_so_name,
            hooks: &self.hooks,
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

    // Skip hooking the dynamic loader and VDSO
    if so_name.contains("/ld-linux")
        || so_name.contains("/ld-musl")
        || so_name.contains("linux-vdso.so")
        || (so_name != "[main executable]" && so_name.contains(context.self_so_name))
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
            let dyn_ptr = (info_ref.dlpi_addr as u64 + phdr.p_vaddr) as *const Elf64_Dyn;
            patch_symbols(
                dyn_ptr,
                info_ref.dlpi_addr as u64,
                context.restore_original,
                context.hooks,
            );
        }
    }

    0
}

unsafe fn patch_symbols(
    dyn_info_struct: *const Elf64_Dyn,
    base: Elf64_Addr,
    restore_original: bool,
    hooks: &[Hook],
) {
    if dyn_info_struct.is_null() {
        debug!("Dynamic info struct is null");
        return;
    }

    debug!("Processing dynamic section at base address: {:#x}", base);
    debug!(
        "Looking for symbols to patch: {:?}",
        hooks.iter().map(|h| h.symbol_name).collect::<Vec<_>>()
    );

    // First try PLT relocations
    match get_jump_table_type(dyn_info_struct) {
        val if val == DT_REL => {
            debug!("Found PLT relocation type: REL");
            overwrite_plt_table::<Elf64_Rel>(dyn_info_struct, base, restore_original, hooks)
        }
        val if val == DT_RELA => {
            debug!("Found PLT relocation type: RELA");
            overwrite_plt_table::<Elf64_Rela>(dyn_info_struct, base, restore_original, hooks)
        }
        val => {
            debug!("Unknown PLT relocation type: {}", val);
        }
    }

    // Then try regular relocations
    overwrite_elf_table::<Elf64_Rel>(dyn_info_struct, base, restore_original, hooks);
    overwrite_elf_table::<Elf64_Rela>(dyn_info_struct, base, restore_original, hooks);
}

unsafe fn get_jump_table_type(dyn_info: *const Elf64_Dyn) -> i64 {
    let mut cur = dyn_info;
    while (*cur).d_tag != DT_NULL {
        if (*cur).d_tag == DT_PLTREL {
            return (*cur).d_un.d_val;
        }
        cur = cur.add(1);
    }
    0
}

trait RelocationEntry {
    unsafe fn find_relocation_table(
        dyn_info_struct: *const Elf64_Dyn,
    ) -> Option<(*const Self, usize)>;
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
    unsafe fn find_relocation_table(
        dyn_info_struct: *const Elf64_Dyn,
    ) -> Option<(*const Elf64_Rel, usize)> {
        let mut rel_ptr: *const Elf64_Rel = ptr::null();
        let mut rel_sz: usize = 0;

        let mut cur = dyn_info_struct;
        while (*cur).d_tag != DT_NULL {
            match (*cur).d_tag {
                tag if tag == DT_REL => {
                    rel_ptr = (*cur).d_un.d_ptr as *const Elf64_Rel;
                }
                tag if tag == DT_RELSZ => {
                    rel_sz = (*cur).d_un.d_val as usize;
                }
                _ => {}
            }
            cur = cur.add(1);
        }
        if !rel_ptr.is_null() && rel_sz > 0 {
            let count = rel_sz / mem::size_of::<Elf64_Rel>();
            Some((rel_ptr, count))
        } else {
            None
        }
    }

    fn offset(&self) -> Elf64_Addr {
        self.r_offset
    }

    fn info(&self) -> u64 {
        self.r_info
    }
}

impl RelocationEntry for Elf64_Rela {
    unsafe fn find_relocation_table(
        dyn_info_struct: *const Elf64_Dyn,
    ) -> Option<(*const Elf64_Rela, usize)> {
        let mut rela_ptr: *const Elf64_Rela = ptr::null();
        let mut rela_sz: usize = 0;

        let mut cur = dyn_info_struct;
        while (*cur).d_tag != DT_NULL {
            match (*cur).d_tag {
                tag if tag == DT_RELA => {
                    rela_ptr = (*cur).d_un.d_ptr as *const Elf64_Rela;
                }
                tag if tag == DT_RELASZ => {
                    rela_sz = (*cur).d_un.d_val as usize;
                }
                _ => {}
            }
            cur = cur.add(1);
        }
        if !rela_ptr.is_null() && rela_sz > 0 {
            let count = rela_sz / mem::size_of::<Elf64_Rela>();
            Some((rela_ptr, count))
        } else {
            None
        }
    }

    fn offset(&self) -> Elf64_Addr {
        self.r_offset
    }

    fn info(&self) -> u64 {
        self.r_info
    }
}

unsafe fn overwrite_elf_table<T>(
    dyn_info_struct: *const Elf64_Dyn,
    base_addr: Elf64_Addr,
    restore_original: bool,
    hooks: &[Hook],
) where
    T: RelocationEntry,
{
    let symbol_table = match SymbolTable::new(dyn_info_struct) {
        Some(table) => table,
        None => {
            debug!("Could not find symbol table");
            return;
        }
    };

    if let Some((table, count)) = T::find_relocation_table(dyn_info_struct) {
        debug!("Found relocation table with {} entries", count);
        let table_slice = slice::from_raw_parts(table, count);
        for reloc in table_slice {
            if !ALLOWED_RELOC_TYPES.contains(&reloc.reloc_type()) {
                continue;
            }
            let symbol_addr = base_addr + reloc.offset() as u64;
            let symbol_idx = reloc.symbol_index();
            if symbol_table.sym_count > 0 && symbol_idx as usize >= symbol_table.sym_count {
                continue;
            }

            // Get symbol from the symbol table
            let sym = &*symbol_table.symtab.add(symbol_idx as usize);
            if let Some(symbol_name) = symbol_table.get_symbol_name(sym.st_name) {
                debug!(
                    "Found symbol: {} at address: {:#x}",
                    symbol_name, symbol_addr
                );
                for hook in hooks {
                    if symbol_name == hook.symbol_name {
                        debug!("Found target symbol {} to patch!", hook.symbol_name);
                        let target_fn = if restore_original {
                            hook.original_fn
                        } else {
                            hook.intercept_fn
                        };
                        patch_symbol(symbol_name, symbol_addr as *mut c_void, target_fn);
                    }
                }
            }
        }
    } else {
        debug!("No relocation table found");
    }
}

unsafe fn patch_symbol(symbol_name: &str, symbol_addr: *mut c_void, new_fn: *mut c_void) {
    debug!(
        "Attempting to patch symbol '{}' at {:p} with new function at {:p}",
        symbol_name, symbol_addr, new_fn
    );

    if unprotect_page(symbol_addr) < 0 {
        error!("Could not unprotect page for symbol '{}'", symbol_name);
        return;
    }

    let typed_ptr = symbol_addr as *mut *mut c_void;
    ptr::write_volatile(typed_ptr, new_fn);
    debug!("Successfully patched symbol '{}'", symbol_name);
}

unsafe fn unprotect_page(addr: *mut c_void) -> c_int {
    let page_size = libc::sysconf(libc::_SC_PAGESIZE) as usize;
    let page_mask = !(page_size - 1);
    let page = (addr as usize & page_mask) as *mut c_void;

    mprotect(page, page_size, PROT_READ | PROT_WRITE)
}

unsafe fn overwrite_plt_table<T>(
    dyn_info_struct: *const Elf64_Dyn,
    base_addr: Elf64_Addr,
    restore_original: bool,
    hooks: &[Hook],
) where
    T: RelocationEntry,
{
    let mut jmprel = ptr::null();
    let mut pltrelsz: usize = 0;
    let mut cur = dyn_info_struct;

    while (*cur).d_tag != DT_NULL {
        match (*cur).d_tag {
            tag if tag == DT_JMPREL => jmprel = (*cur).d_un.d_ptr as *const T,
            tag if tag == DT_PLTRELSZ => pltrelsz = (*cur).d_un.d_val as usize,
            _ => {}
        }
        cur = cur.add(1);
    }

    if !jmprel.is_null() && pltrelsz > 0 {
        let count = pltrelsz / mem::size_of::<T>();
        debug!("Found PLT table with {} entries", count);
        let table_slice = slice::from_raw_parts(jmprel, count);

        let symbol_table = match SymbolTable::new(dyn_info_struct) {
            Some(table) => table,
            None => {
                debug!("Could not find symbol table");
                return;
            }
        };

        for reloc in table_slice {
            if !ALLOWED_RELOC_TYPES.contains(&reloc.reloc_type()) {
                continue;
            }
            let symbol_addr = base_addr + reloc.offset() as u64;
            let symbol_idx = reloc.symbol_index();
            if symbol_table.sym_count > 0 && symbol_idx as usize >= symbol_table.sym_count {
                continue;
            }

            let sym = &*symbol_table.symtab.add(symbol_idx as usize);
            if let Some(symbol_name) = symbol_table.get_symbol_name(sym.st_name) {
                debug!(
                    "Found PLT symbol: {} at address: {:#x}",
                    symbol_name, symbol_addr
                );
                for hook in hooks {
                    if symbol_name == hook.symbol_name {
                        debug!("Found target PLT symbol {} to patch!", hook.symbol_name);
                        let target_fn = if restore_original {
                            hook.original_fn
                        } else {
                            hook.intercept_fn
                        };
                        patch_symbol(symbol_name, symbol_addr as *mut c_void, target_fn);
                    }
                }
            }
        }
    }
}
