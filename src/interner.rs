use crate::sync::Mutex;
use hashbrown::HashMap;
use once_cell::sync::Lazy;
use std::sync::{Arc, Weak};

pub(crate) static STRING_INTERNER: Lazy<Mutex<HashMap<String, Weak<str>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Frames own their strings. Weak entries share live strings without forcing
/// every filename seen by a long-running profiler to live forever.
pub fn intern(s: &str) -> Arc<str> {
    let mut interner = STRING_INTERNER.lock();
    if let Some(interned) = interner.get(s).and_then(Weak::upgrade) {
        return interned;
    }
    // Amortize pruning of dead keys; reset also clears this index outright.
    if interner.len() >= 1024 && interner.len().is_multiple_of(1024) {
        interner.retain(|_, value| value.strong_count() != 0);
    }
    let owned: Arc<str> = Arc::from(s);
    interner.insert(s.to_owned(), Arc::downgrade(&owned));
    owned
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_shares_live_strings_without_retaining_them() {
        let a = intern("test_intern_unique_string_xyz");
        let b = intern("test_intern_unique_string_xyz");
        assert!(Arc::ptr_eq(&a, &b));
        let weak = Arc::downgrade(&a);
        drop(a);
        drop(b);
        assert!(weak.upgrade().is_none());
    }
}
