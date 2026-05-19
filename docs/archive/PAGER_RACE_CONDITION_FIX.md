# Pager Race Condition Fix - Issue Nanostore-49y

## Problem Summary

Critical race condition in Pager's page allocation logic allowed multiple threads to receive the same page ID under high contention, leading to:
- Data corruption (multiple pages writing to same location)
- Data loss (pages overwriting each other)
- Database integrity compromised

## Root Cause Analysis

### The Race Condition

The issue was in `Superblock::allocate_new_page()` method:

```rust
// BEFORE (BUGGY CODE):
pub fn allocate_new_page(&mut self) -> PageId {
    let page_id = self.next_page_id;  // Read
    self.next_page_id += 1;            // Increment
    self.total_pages += 1;
    page_id
}
```

**Problem**: Even though `pagefile.rs` held both `free_list` and `superblock` locks atomically, the `next_page_id` field itself was a plain `u64`. When multiple threads called this method:

1. Thread A reads `next_page_id = 5`
2. Thread B reads `next_page_id = 5` (before Thread A increments)
3. Thread A increments to 6
4. Thread B increments to 6
5. Both threads return page ID 5 → **DUPLICATE!**

This is a classic Time-Of-Check-Time-Of-Use (TOCTOU) race condition.

### Evidence

The test `test_free_list_contention` demonstrated the bug:
- 16 threads × 20 allocations = 320 total allocations
- Only 41 unique page IDs were generated
- 279 duplicate page IDs!

## Solution

### Atomic Page ID Generation

Changed `next_page_id` from a plain `u64` to `Arc<AtomicU64>`:

```rust
// AFTER (FIXED CODE):
pub struct Superblock {
    // ... other fields ...
    /// Next page ID to allocate (grows the database) - ATOMIC for thread safety
    next_page_id: Arc<AtomicU64>,
    // ... other fields ...
}

pub fn allocate_new_page(&mut self) -> PageId {
    // Atomically fetch the current value and increment it
    // This is the KEY FIX - fetch_add is atomic!
    let page_id = self.next_page_id.fetch_add(1, Ordering::SeqCst);
    self.total_pages += 1;
    page_id
}
```

### Key Changes

1. **Atomic Field**: `next_page_id: Arc<AtomicU64>` instead of `next_page_id: PageId`
2. **Atomic Operation**: `fetch_add(1, Ordering::SeqCst)` is a single atomic CPU instruction
3. **Clone Implementation**: Custom `Clone` that clones the `Arc`, sharing the same atomic counter
4. **Accessor Method**: Added `next_page_id()` method for reading the current value

### Why This Works

`AtomicU64::fetch_add()` is a **single atomic CPU instruction** that:
1. Reads the current value
2. Increments it
3. Returns the old value

All in one indivisible operation. No other thread can interleave between these steps.

## Files Modified

1. **src/pager/superblock.rs**
   - Changed `next_page_id` field to `Arc<AtomicU64>`
   - Updated `new()` to initialize with `Arc::new(AtomicU64::new(2))`
   - Added `next_page_id()` accessor method
   - Modified `allocate_new_page()` to use `fetch_add()`
   - Updated `to_bytes()` to load atomic value
   - Updated `from_bytes()` to create atomic from value
   - Added custom `Clone` implementation
   - Fixed all test cases to use `next_page_id()` accessor

## Testing

### Before Fix
```
test test_free_list_contention ... FAILED
Expected: 320 unique page IDs
Actual: 41 unique page IDs (279 duplicates!)
```

### After Fix
```
test test_free_list_contention ... ok
All 14 concurrency tests pass
All 41 pager unit tests pass
```

### Test Coverage

All concurrency tests now pass:
- `test_concurrent_allocation_2_threads`
- `test_concurrent_allocation_4_threads`
- `test_concurrent_allocation_8_threads`
- `test_concurrent_allocation_16_threads`
- `test_concurrent_reads_different_pages`
- `test_concurrent_writes_different_pages`
- `test_concurrent_mixed_read_write`
- `test_concurrent_allocation_deallocation`
- `test_free_list_contention` ← **This was the smoking gun test**
- `test_data_integrity_concurrent_operations`
- `test_no_duplicate_page_ids_stress`
- `test_concurrent_allocation_different_page_types`
- `test_concurrent_free_list_operations`
- `test_concurrent_sync_operations`

## Performance Impact

**Minimal to None**:
- `AtomicU64::fetch_add()` is a single CPU instruction (LOCK XADD on x86)
- No additional locks or synchronization needed
- The existing RwLock on superblock is still held, so no contention increase
- Memory overhead: +8 bytes for Arc pointer (negligible)

## Thread Safety Guarantees

With this fix:
1. ✅ **No duplicate page IDs**: Atomic fetch_add ensures uniqueness
2. ✅ **No data corruption**: Each page gets a unique ID
3. ✅ **No data loss**: Pages never overwrite each other
4. ✅ **Scalable**: Works with any number of concurrent threads
5. ✅ **Lock-free page ID generation**: No additional locks needed

## Related Issues

This fix resolves:
- Issue Nanostore-49y: "Pager: Race condition in page allocation causes duplicate page IDs"

## Future Considerations

The free list operations (`pop_page()` and `push_page()`) are already protected by the RwLock and don't need atomic operations because:
1. They're always called while holding the lock
2. They modify a Vec, which isn't thread-safe anyway
3. The lock provides the necessary synchronization

The atomic `next_page_id` is needed specifically because it's used for **growing** the database, which is a monotonically increasing counter that benefits from lock-free atomic operations.

## Conclusion

This fix eliminates a critical race condition that could cause data corruption in production. The solution is minimal, efficient, and maintains backward compatibility with the serialization format.

**Status**: ✅ FIXED and TESTED
**Date**: 2026-05-08
**Author**: Bob (AI Assistant)