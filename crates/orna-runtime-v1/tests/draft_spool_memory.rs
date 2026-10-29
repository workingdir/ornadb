//! An edit draft spools its candidate to disk, so a large candidate never sits
//! in memory. Writing 64 MiB in 1 MiB chunks and reading it back must keep the
//! peak live heap far below the candidate size.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
};

use orna_runtime_v1::vfs::{
    DraftQuota, DraftStorage, EditDraft, SnapshotPin, VfsFileSnapshot, VfsRepositoryCache,
};
use tempfile::TempDir;

const CHUNK_BYTES: usize = 1 << 20;
const CHUNKS: usize = 64;
const CANDIDATE_BYTES: usize = CHUNK_BYTES * CHUNKS;
/// The live heap may hold a few chunks at a time, never the whole candidate.
const PEAK_LIMIT_BYTES: i64 = 8 * CHUNK_BYTES as i64;

// Signed: a block allocated before the measurement window can be freed inside
// it, which must drive the live count down without wrapping.
static LIVE_BYTES: AtomicI64 = AtomicI64::new(0);
static PEAK_BYTES: AtomicI64 = AtomicI64::new(0);

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
        LIVE_BYTES.fetch_sub(layout.size() as i64, Ordering::Relaxed);
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let result = unsafe { System.realloc(pointer, layout, new_size) };
        if !result.is_null() {
            LIVE_BYTES.fetch_sub(layout.size() as i64, Ordering::Relaxed);
            track_grow(new_size);
        }
        result
    }
}

fn track_grow(size: usize) {
    let live = LIVE_BYTES.fetch_add(size as i64, Ordering::Relaxed) + size as i64;
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

    // Measure only the candidate work: capture the live heap as it stands after
    // setup and track the peak relative to it, so setup blocks freed inside the
    // window cannot drive the count negative or hide growth.
    let baseline_live = LIVE_BYTES.load(Ordering::Relaxed);
    PEAK_BYTES.store(baseline_live, Ordering::Relaxed);

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

    let peak = PEAK_BYTES.load(Ordering::Relaxed) - baseline_live;
    assert!(
        peak < PEAK_LIMIT_BYTES,
        "peak live heap {peak} bytes reached the {PEAK_LIMIT_BYTES}-byte limit; \
         the candidate was held in memory"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn sixty_four_mib_scratch_fsync_stays_bounded_in_memory() {
    let spool = TempDir::new().unwrap();
    let cache = VfsRepositoryCache::in_runtime_root(spool.path()).expect("runtime root");
    let baseline = Arc::new(VfsFileSnapshot::new(
        SnapshotPin::capture(Arc::new(7_u64)),
        Vec::<u8>::new(),
    ));
    let destination = cache
        .managed_file(baseline)
        .await
        .expect("scope the destination baseline");
    let scratch = destination.begin_temporary_replacement(CANDIDATE_BYTES).await;
    for chunk in 0..CHUNKS {
        scratch
            .write_at((chunk * CHUNK_BYTES) as u64, &pattern(chunk))
            .await
            .expect("spool a chunk");
    }

    let baseline_live = LIVE_BYTES.load(Ordering::Relaxed);
    PEAK_BYTES.store(baseline_live, Ordering::Relaxed);
    scratch
        .fsync_scratch_with(|_, snapshot| async move {
            assert_eq!(snapshot.len(), CANDIDATE_BYTES as u64);
            for chunk in 0..CHUNKS {
                let range = snapshot
                    .read_range((chunk * CHUNK_BYTES) as u64, CHUNK_BYTES)
                    .expect("read a snapshot chunk");
                assert_eq!(range, pattern(chunk), "chunk {chunk} streams from the snapshot");
            }
            Ok::<(), ()>(())
        })
        .await
        .expect("persist the scratch candidate");

    let peak = PEAK_BYTES.load(Ordering::Relaxed) - baseline_live;
    assert!(
        peak < PEAK_LIMIT_BYTES,
        "scratch fsync peak live heap {peak} bytes reached the {PEAK_LIMIT_BYTES}-byte limit; \
         the whole candidate was copied for persistence"
    );
}
