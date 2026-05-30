use std::ops::{Deref, DerefMut};

/// A wrapper that tracks whether the inner value has been mutably accessed
/// since the last call to [`take_dirty`](Tracked::take_dirty).
///
/// Intended for coarse-grained invalidation: any mutable access sets the
/// dirty flag, which a render path can poll to decide whether to invalidate a
/// derived cache.  This deliberately over-fires (a mutable borrow that does
/// not change visible content still sets the flag), but that is acceptable
/// when rebuilding the cache is cheap.
pub struct Tracked<T> {
    inner: T,
    dirty: bool,
}

impl<T> Tracked<T> {
    pub fn new(inner: T) -> Self {
        Self {
            inner,
            dirty: false,
        }
    }

    /// Returns `true` and clears the flag if the value has been mutably
    /// accessed since the last call, otherwise returns `false`.
    pub fn take_dirty(&mut self) -> bool {
        let was = self.dirty;
        self.dirty = false;
        was
    }
}

impl<T> Deref for Tracked<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.inner
    }
}

impl<T> DerefMut for Tracked<T> {
    fn deref_mut(&mut self) -> &mut T {
        self.dirty = true;
        &mut self.inner
    }
}
