use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

pub(crate) static STRING_INTERNER: Lazy<Mutex<HashMap<String, Arc<String>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Interns a string and returns a `'static` reference to the single shared copy.
///
/// SAFETY invariant behind the transmute: entries are NEVER removed from
/// STRING_INTERNER, so the `Arc<String>`'s heap buffer lives for the rest of
/// the process. The Arc indirection keeps that buffer address stable across
/// map rehashes. If eviction is ever added, this transmute becomes a
/// use-after-free and must be redesigned first.
pub fn intern(s: &str) -> Cow<'static, str> {
    let mut interner = STRING_INTERNER.lock();
    if let Some(interned) = interner.get(s) {
        let static_str: &'static str = unsafe { std::mem::transmute(interned.as_str()) };
        return Cow::Borrowed(static_str);
    }
    let owned = Arc::new(s.to_string());
    let static_str: &'static str = unsafe { std::mem::transmute(owned.as_str()) };
    interner.insert(s.to_string(), owned);
    Cow::Borrowed(static_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_always_returns_borrowed_and_stable() {
        let a = intern("test_intern_unique_string_xyz");
        let b = intern("test_intern_unique_string_xyz");
        assert!(
            matches!(a, Cow::Borrowed(_)),
            "first intern must be Borrowed"
        );
        assert!(matches!(b, Cow::Borrowed(_)));
        assert_eq!(
            a.as_ptr(),
            b.as_ptr(),
            "same string must intern to same address"
        );
    }
}
