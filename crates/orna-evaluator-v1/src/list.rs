//! Shared immutable list storage for evaluator values.
//!
//! A [`List`] is a view of the first `len` slots of an append-only buffer.
//! Cloning a list shares its buffer, so passing, binding or capturing a list
//! never copies its elements. Appending to the longest view of a buffer that
//! still has spare capacity writes the new elements in place; every other
//! append copies into a fresh buffer with doubled capacity. Accumulating a list
//! one element at a time therefore costs amortised O(1) per element, while
//! every view keeps observing the same immutable prefix.

use std::cell::UnsafeCell;
use std::fmt;
use std::mem::{ManuallyDrop, MaybeUninit};
use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::Value;

/// Append-only slots shared by every [`List`] view cut from them.
struct Buffer {
    slots: Box<[UnsafeCell<MaybeUninit<Value>>]>,
    /// Slots `0..claimed` are initialised and never written again. An appender
    /// claims slots by advancing this counter from the end of its own view and
    /// initialises them before any view that covers them exists.
    claimed: AtomicUsize,
}

// SAFETY: initialised slots are only ever read. Each uninitialised slot is
// written exactly once, by the appender whose compare-and-swap claimed it,
// before any view covering that slot is created. Views reach other threads
// only through synchronising operations, which order those writes first, so
// shared access never reads a slot outside its own view.
unsafe impl Sync for Buffer {}

const _: () = {
    const fn shared<T: Send + Sync>() {}
    shared::<Value>();
};

impl Buffer {
    /// An exclusively owned buffer with `capacity` uninitialised slots.
    fn with_capacity(capacity: usize) -> Self {
        Self {
            slots: (0..capacity)
                .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
                .collect(),
            claimed: AtomicUsize::new(0),
        }
    }

    /// Initialises the next slot of a buffer that no view shares yet.
    fn push_exclusive(&mut self, value: Value) {
        let index = *self.claimed.get_mut();
        self.slots[index].get_mut().write(value);
        *self.claimed.get_mut() = index + 1;
    }

    /// The initialised prefix `0..len`.
    ///
    /// # Safety
    /// `len` must not exceed the number of slots initialised so far.
    unsafe fn prefix(&self, len: usize) -> &[Value] {
        // SAFETY: `UnsafeCell<MaybeUninit<Value>>` has the layout of `Value`,
        // and the caller guarantees the first `len` slots are initialised.
        // Initialised slots are never written again.
        unsafe { std::slice::from_raw_parts(self.slots.as_ptr().cast::<Value>(), len) }
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        let claimed = *self.claimed.get_mut();
        for slot in &mut self.slots[..claimed] {
            // SAFETY: every claimed slot was initialised immediately after its
            // claim, and the buffer is being dropped exactly once.
            unsafe { slot.get_mut().assume_init_drop() };
        }
    }
}

/// An immutable list value whose clones share storage.
pub(crate) struct List {
    buffer: Arc<Buffer>,
    len: usize,
}

impl List {
    pub(crate) fn new() -> Self {
        Self::from(Vec::new())
    }

    pub(crate) fn as_slice(&self) -> &[Value] {
        // SAFETY: a view is created only after its whole prefix is initialised.
        unsafe { self.buffer.prefix(self.len) }
    }

    /// Returns this list followed by `tail`, and how many existing elements
    /// were copied to build it. Nothing is copied when this view ends at the
    /// buffer's last claimed slot and the buffer has room for `tail`.
    pub(crate) fn concat(&self, tail: &[Value]) -> (Self, usize) {
        if tail.is_empty() {
            return (self.clone(), 0);
        }
        let len = self.len + tail.len();
        if len <= self.buffer.slots.len() && self.buffer.claimed.load(Ordering::Acquire) == self.len
        {
            // Clone before claiming so that no panic can leave a claimed slot
            // uninitialised.
            let tail = tail.to_vec();
            if self
                .buffer
                .claimed
                .compare_exchange(self.len, len, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                for (slot, value) in self.buffer.slots[self.len..len].iter().zip(tail) {
                    // SAFETY: the successful exchange gave this call exclusive
                    // ownership of slots `self.len..len`, and no view covers
                    // them until the list returned below.
                    unsafe { (*slot.get()).write(value) };
                }
                let list = Self {
                    buffer: Arc::clone(&self.buffer),
                    len,
                };
                return (list, 0);
            }
            return (self.copied_with(&tail, len), self.len);
        }
        (self.copied_with(tail, len), self.len)
    }

    /// A fresh buffer holding this list and `tail`, with room to grow.
    fn copied_with(&self, tail: &[Value], len: usize) -> Self {
        let mut buffer = Buffer::with_capacity(len.max(4).saturating_mul(2));
        for value in self.iter().chain(tail) {
            buffer.push_exclusive(value.clone());
        }
        Self {
            buffer: Arc::new(buffer),
            len,
        }
    }

    /// Moves the elements out when this is the only view of its buffer, and
    /// clones them otherwise.
    pub(crate) fn into_vec(self) -> Vec<Value> {
        let len = self.len;
        match Arc::try_unwrap(self.buffer) {
            Ok(mut buffer) => {
                let claimed = std::mem::replace(buffer.claimed.get_mut(), 0);
                let mut values = Vec::with_capacity(len);
                for (index, slot) in buffer.slots[..claimed].iter_mut().enumerate() {
                    // SAFETY: slots below `claimed` are initialised. Resetting
                    // `claimed` first means the buffer drops none of them, so
                    // each is read or dropped here exactly once.
                    let value = unsafe { slot.get_mut().assume_init_read() };
                    if index < len {
                        values.push(value);
                    }
                }
                values
            }
            Err(buffer) => {
                // SAFETY: `len` slots were initialised before this view existed.
                unsafe { buffer.prefix(len) }.to_vec()
            }
        }
    }
}

impl Default for List {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Vec<Value>> for List {
    fn from(values: Vec<Value>) -> Self {
        let len = values.len();
        let mut values = ManuallyDrop::new(values);
        let (pointer, capacity) = (values.as_mut_ptr(), values.capacity());
        // SAFETY: the allocation holds `capacity` slots with the layout of
        // `Value`; the first `len` stay initialised, and slots typed as
        // `MaybeUninit` impose no initialisation requirement. Ownership of the
        // allocation moves to the new vector because `values` is never dropped.
        let slots: Vec<UnsafeCell<MaybeUninit<Value>>> =
            unsafe { Vec::from_raw_parts(pointer.cast(), capacity, capacity) };
        Self {
            buffer: Arc::new(Buffer {
                slots: slots.into_boxed_slice(),
                claimed: AtomicUsize::new(len),
            }),
            len,
        }
    }
}

impl FromIterator<Value> for List {
    fn from_iter<I: IntoIterator<Item = Value>>(values: I) -> Self {
        Self::from(values.into_iter().collect::<Vec<_>>())
    }
}

impl IntoIterator for List {
    type Item = Value;
    type IntoIter = std::vec::IntoIter<Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.into_vec().into_iter()
    }
}

impl<'list> IntoIterator for &'list List {
    type Item = &'list Value;
    type IntoIter = std::slice::Iter<'list, Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
}

impl Deref for List {
    type Target = [Value];

    fn deref(&self) -> &[Value] {
        self.as_slice()
    }
}

impl Clone for List {
    fn clone(&self) -> Self {
        Self {
            buffer: Arc::clone(&self.buffer),
            len: self.len,
        }
    }
}

impl PartialEq for List {
    fn eq(&self, other: &Self) -> bool {
        (Arc::ptr_eq(&self.buffer, &other.buffer) && self.len == other.len)
            || self.as_slice() == other.as_slice()
    }
}

impl Eq for List {}

impl fmt::Debug for List {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_list().entries(self.iter()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::List;
    use crate::Value;
    use num_bigint::BigInt;

    fn int(value: i64) -> Value {
        Value::Int(BigInt::from(value))
    }

    fn ints(values: &[i64]) -> Vec<Value> {
        values.iter().copied().map(int).collect()
    }

    #[test]
    fn appending_to_the_longest_view_reuses_its_buffer() {
        let mut list = List::new();
        let mut copied = 0;
        for value in 0..1_000 {
            let (next, moved) = list.concat(&[int(value)]);
            copied += moved;
            list = next;
        }
        assert_eq!(list.len(), 1_000);
        assert_eq!(list[999], int(999));
        // Doubling growth copies each prefix at most once per capacity step.
        assert!(copied < 2_000, "copied {copied} elements");
    }

    /// A list built by appending, so its buffer has spare capacity.
    fn spare(values: &[i64]) -> List {
        List::new().concat(&ints(values)).0
    }

    #[test]
    fn every_view_keeps_its_own_prefix() {
        let base = spare(&[1, 2]);
        let (left, left_copied) = base.concat(&ints(&[3]));
        let (right, right_copied) = base.concat(&ints(&[4]));
        assert_eq!(base.as_slice(), ints(&[1, 2]).as_slice());
        assert_eq!(left.as_slice(), ints(&[1, 2, 3]).as_slice());
        assert_eq!(right.as_slice(), ints(&[1, 2, 4]).as_slice());
        // The first sibling extends the shared buffer; the second copies.
        assert_eq!((left_copied, right_copied), (0, 2));
    }

    #[test]
    fn empty_tail_and_equality_share_the_view() {
        let base = List::from(ints(&[1, 2]));
        let (same, copied) = base.concat(&[]);
        assert_eq!(copied, 0);
        assert_eq!(same, base);
        assert_eq!(List::from(ints(&[1, 2])), base);
        assert_ne!(List::from(ints(&[2, 1])), base);
    }

    #[test]
    fn into_vec_moves_unique_views_and_clones_shared_ones() {
        let (list, _) = List::from(ints(&[1])).concat(&ints(&[2, 3]));
        let (shorter, _) = List::from(ints(&[7])).concat(&ints(&[8]));
        let shared = shorter.clone();
        assert_eq!(list.into_vec(), ints(&[1, 2, 3]));
        assert_eq!(shorter.into_vec(), ints(&[7, 8]));
        assert_eq!(shared.as_slice(), ints(&[7, 8]).as_slice());
    }

    #[test]
    fn a_view_shorter_than_its_buffer_drops_the_longer_elements_once() {
        let base = spare(&[0, 1]);
        let (longer, copied) = base.concat(&ints(&[2, 3]));
        assert_eq!(copied, 0);
        drop(longer);
        // The base view is now the only owner of a buffer with four elements.
        assert_eq!(base.into_vec(), ints(&[0, 1]));
    }

    #[test]
    fn concurrent_appends_to_one_view_keep_every_result_intact() {
        let base = spare(&[0]);
        let results = std::thread::scope(|scope| {
            let handles = (1..=8)
                .map(|value| {
                    let base = base.clone();
                    scope.spawn(move || base.concat(&[int(value)]).0)
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("append thread"))
                .collect::<Vec<_>>()
        });
        for (index, list) in results.iter().enumerate() {
            assert_eq!(list.as_slice(), ints(&[0, index as i64 + 1]).as_slice());
        }
        assert_eq!(base.as_slice(), ints(&[0]).as_slice());
    }
}
