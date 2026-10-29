//! An edit draft spools its candidate to disk, so a large candidate never sits
//! in memory. Writing 64 MiB in 1 MiB chunks and reading it back must keep the
//! peak live heap far below the candidate size.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use orna_runtime_v1::vfs::{DraftQuota, DraftStorage, EditDraft, SnapshotPin, VfsFileSnapshot};
use tempfile::TempDir;

const CHUNK_BYTES: usize = 1 << 20;
const CHUNKS: usize = 64;
const CANDIDATE_BYTES: usize = CHUNK_BYTES * CHUNKS;
/// The live heap may hold a few chunks at a time, never the whole candidate.
const PEAK_LIMIT_BYTES: u64 = 8 * CHUNK_BYTES as u64;

static LIVE_BYTES: AtomicU64 = AtomicU64::new(0);
static PEAK_BYTES: AtomicU64 = AtomicU64::new(0);

struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let result = unsafe { System.alloc(layout) };
        if !result.is_null() {
            track_grow(layout.size());
        }
        result
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        LIVE_BYTES.fetch_sub(layout.size() as u64, Ordering::Relaxed);
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let result = unsafe { System.realloc(pointer, layout, new_size) };
        if !result.is_null() {
            LIVE_BYTES.fetch_sub(layout.size() as u64, Ordering::Relaxed);
            track_grow(new_size);
        }
        result
    }
}

fn track_grow(size: usize) {
    let live = LIVE_BYTES.fetch_add(size as u64, Ordering::Relaxed) + size as u64;
    PEAK_BYTES.fetch_max(live, Ordering::Relaxed);
}

#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

fn pattern(chunk: usize) -> Vec<u8> {
    (0..CHUNK_BYTES)
        .map(|index| (chunk.wrapping_mul(31).wrapping_add(index) % 251) as u8)
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn sixty_four_mib_candidate_stays_bounded_in_memory() {
    let spool = TempDir::new().unwrap();
    let quota = Arc::new(DraftQuota::new(1 << 30));
    let storage = DraftStorage::new(spool.path(), quota);
    let baseline = Arc::new(VfsFileSnapshot::new(
        SnapshotPin::capture(Arc::new(7_u64)),
        Vec::<u8>::new(),
    ));
    let draft = EditDraft::open(baseline, CANDIDATE_BYTES, storage);

    // Measure only the candidate work, not the test harness setup above.
    LIVE_BYTES.store(0, Ordering::Relaxed);
    PEAK_BYTES.store(0, Ordering::Relaxed);

    for chunk in 0..CHUNKS {
        let bytes = pattern(chunk);
        draft
            .write_at((chunk * CHUNK_BYTES) as u64, &bytes)
            .await
            .expect("spool a chunk");
    }
    assert_eq!(draft.candidate_len().await, CANDIDATE_BYTES as u64);

    for chunk in 0..CHUNKS {
        let read = draft
            .read_range((chunk * CHUNK_BYTES) as u64, CHUNK_BYTES)
            .await
            .expect("read a chunk back");
        assert_eq!(read, pattern(chunk), "chunk {chunk} round-trips through the spool");
    }

    let peak = PEAK_BYTES.load(Ordering::Relaxed);
    assert!(
        peak < PEAK_LIMIT_BYTES,
        "peak live heap {peak} bytes reached the {PEAK_LIMIT_BYTES}-byte limit; \
         the candidate was held in memory"
    );
}
