use once_cell::sync::Lazy;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

static STRING_INTERNER: Lazy<Mutex<HashMap<String, Arc<String>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

pub fn intern(s: &str) -> Cow<'static, str> {
    let mut interner = STRING_INTERNER.lock().unwrap();
    if let Some(interned) = interner.get(s) {
        let static_str: &'static str = unsafe { std::mem::transmute(interned.as_str()) };
        Cow::Borrowed(static_str)
    } else {
        let owned = Arc::new(s.to_string());
        interner.insert(s.to_string(), owned);
        Cow::Owned(s.to_string())
    }
}
