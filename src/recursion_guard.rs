use std::cell::Cell;

thread_local! {
    static IN_MALLOC: Cell<bool> = Cell::new(false);
}

pub struct MallocRecursionGuard {
    was_in_malloc: bool,
}

impl MallocRecursionGuard {
    pub fn new() -> Self {
        let was_in_malloc = IN_MALLOC.with(|in_malloc| {
            let prev = in_malloc.get();
            in_malloc.set(true);
            prev
        });
        Self { was_in_malloc }
    }

    pub fn was_in_malloc(&self) -> bool {
        self.was_in_malloc
    }
}

impl Drop for MallocRecursionGuard {
    fn drop(&mut self) {
        IN_MALLOC.with(|in_malloc| in_malloc.set(false));
    }
}
