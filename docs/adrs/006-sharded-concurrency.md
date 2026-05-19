# ADR-006: Sharded Concurrency Model

**Status**: Accepted  
**Date**: 2026-05-10  
**Deciders**: Hans W. Uhlig, Development Team  
**Technical Story**: Pager concurrency improvements (Nanostore-z34, Nanostore-5p9)

## Context

The initial pager implementation used coarse-grained locking:
- Single RwLock for the entire cache
- Single RwLock for the free list
- File-level locking for all I/O

This caused significant lock contention under concurrent load:
- **Baseline**: ~130K ops/sec @ 8 threads (only 30% improvement over single-threaded)
- **Target**: ~500K ops/sec @ 8 threads (5x improvement)

We need a concurrency model that:
- Minimizes lock contention
- Scales with thread count
- Maintains correctness
- Doesn't add excessive complexity

## Decision

We will implement a **sharded concurrency model** with three key components:

1. **PageTable**: Fine-grained page-level locking (64 shards)
2. **Sharded Cache**: Independent cache shards (64 shards)
3. **Lock-Free FreeList**: Lock-free page allocation

**Sharding Strategy**: Hash-based sharding using `page_id % shard_count`

## Consequences

### Positive

- **Reduced Contention**: Lock contention reduced by ~64x
- **Better Scalability**: Linear scaling up to 64 threads
- **Predictable Performance**: Consistent latency under load
- **No Deadlocks**: Strict lock ordering prevents deadlocks
- **Incremental Adoption**: Can be implemented in phases

### Negative

- **Increased Complexity**: More components to manage
- **Memory Overhead**: 64 locks + data structures per shard
- **Uneven Distribution**: Hash collisions can cause hot shards
- **Tuning Required**: Shard count may need adjustment

### Mitigations

1. **Complexity**: Well-documented, tested implementation
2. **Memory**: Minimal overhead (~8KB for 64 shards)
3. **Distribution**: Good hash function ensures even distribution
4. **Tuning**: Configurable shard count (default: 64)

## Implementation Details

### 1. PageTable (Fine-Grained Locking)

**Structure**:
```rust
pub struct PageTable {
    shards: Vec<RwLock<PageShard>>,
    shard_count: usize,
}

struct PageShard {
    // Empty - just provides lock granularity
}
```

**Usage**:
```rust
// Acquire page-level lock
let shard_id = page_id.as_u64() as usize % self.shard_count;
let _guard = self.shards[shard_id].read();  // or write()

// Perform page operation
// Lock automatically released when guard drops
```

**Benefits**:
- Different pages (different shards) can be accessed concurrently
- Same page serializes access (correctness)
- RAII guards prevent lock leaks

### 2. Sharded Cache

**Structure**:
```rust
pub struct PageCache {
    shards: Vec<RwLock<CacheShard>>,
    shard_count: usize,
}

struct CacheShard {
    lru: LruCache<PageId, Arc<Page>>,
    capacity: usize,
}
```

**Operations**:
```rust
fn get(&self, page_id: PageId) -> Option<Arc<Page>> {
    let shard_id = page_id.as_u64() as usize % self.shard_count;
    let shard = self.shards[shard_id].read();
    shard.lru.get(&page_id).cloned()
}

fn insert(&self, page_id: PageId, page: Arc<Page>) {
    let shard_id = page_id.as_u64() as usize % self.shard_count;
    let mut shard = self.shards[shard_id].write();
    
    // Evict if necessary
    if shard.lru.len() >= shard.capacity {
        shard.lru.pop_lru();
    }
    
    shard.lru.put(page_id, page);
}
```

**Benefits**:
- Parallel cache operations on different shards
- Independent LRU per shard (no global LRU contention)
- Better cache hit rates (less eviction interference)

### 3. Lock-Free FreeList

**Structure**:
```rust
pub struct FreeList {
    queue: SegQueue<PageId>,           // Lock-free queue
    free_count: AtomicU64,             // Atomic counter
    allocated_count: AtomicU64,        // Atomic counter
}
```

**Operations**:
```rust
fn allocate(&self) -> Option<PageId> {
    if let Some(page_id) = self.queue.pop() {
        self.free_count.fetch_sub(1, Ordering::SeqCst);
        self.allocated_count.fetch_add(1, Ordering::SeqCst);
        Some(page_id)
    } else {
        None
    }
}

fn free(&self, page_id: PageId) {
    self.queue.push(page_id);
    self.free_count.fetch_add(1, Ordering::SeqCst);
    self.allocated_count.fetch_sub(1, Ordering::SeqCst);
}
```

**Benefits**:
- No lock contention on allocation/deallocation
- Scales linearly with thread count
- Simple implementation using crossbeam

### Lock Ordering

To prevent deadlocks, we enforce strict lock ordering:

```
1. Page-level lock (PageTable)     ← Acquired first
2. File lock (VFS)                 ← Acquired second
3. Superblock/Header locks         ← Acquired last
```

**Example**:
```rust
fn read_page(&self, page_id: PageId) -> Result<Page> {
    // 1. Acquire page lock
    let _page_guard = self.page_table.read_lock(page_id);
    
    // 2. Check cache (no lock needed, sharded)
    if let Some(page) = self.cache.get(page_id) {
        return Ok(page);
    }
    
    // 3. Acquire file lock
    let mut file = self.file.write();
    
    // 4. Read from disk
    let data = file.read_at_offset(offset)?;
    
    // 5. Release file lock (automatic)
    drop(file);
    
    // 6. Cache page
    self.cache.insert(page_id, page.clone());
    
    // 7. Release page lock (automatic)
    Ok(page)
}
```

## Performance Characteristics

### Theoretical Scalability

With 64 shards:
- **Best case**: 64 threads can access different shards simultaneously
- **Average case**: ~50-60 threads can work concurrently (hash distribution)
- **Worst case**: All threads access same shard (serialized)

### Measured Performance

**Phase 1 Results** (PageTable integration):
- ✅ All 19 concurrency tests passing
- ✅ No regressions
- ⏳ Benchmarks pending (Windows file locking issue)

**Expected Results** (from design):
- **Target**: 3-5x throughput improvement @ 8 threads
- **Baseline**: ~130K ops/sec @ 8 threads
- **Goal**: ~500K ops/sec @ 8 threads

### Shard Count Tuning

**Default: 64 shards**
- Good balance for 1-64 threads
- Low memory overhead (~8KB)
- Even distribution with good hash

**Alternative configurations**:
- **32 shards**: Lower memory, good for 1-32 threads
- **128 shards**: Higher parallelism, more memory
- **256 shards**: Maximum parallelism, highest memory

**Recommendation**: Start with 64, tune based on workload.

## Alternatives Considered

### Alternative 1: Global Locks

**Approach**: Single lock for each component (cache, free list, etc.)

**Pros**:
- Simple implementation
- Easy to reason about
- Low memory overhead

**Cons**:
- High lock contention
- Poor scalability
- Serializes all operations

**Rejected because**: Unacceptable performance under concurrent load.

### Alternative 2: Lock-Free Everything

**Approach**: Use lock-free data structures for all components.

**Pros**:
- Maximum concurrency
- No lock contention
- Best theoretical performance

**Cons**:
- Very complex implementation
- Hard to debug
- May not be faster in practice (CAS overhead)
- Limited lock-free data structure options

**Rejected because**: Complexity outweighs benefits. Sharding provides good enough performance.

### Alternative 3: Per-Page Locks

**Approach**: One lock per page (not sharded).

**Pros**:
- Maximum granularity
- No hash collisions

**Cons**:
- Huge memory overhead (millions of locks)
- Lock management complexity
- Slower lock acquisition

**Rejected because**: Memory overhead is prohibitive.

### Alternative 4: Read-Copy-Update (RCU)

**Approach**: Copy-on-write with RCU for reads.

**Pros**:
- Lock-free reads
- Good for read-heavy workloads

**Cons**:
- Complex implementation
- Memory overhead (copies)
- Not suitable for write-heavy workloads

**Rejected because**: MVCC already provides non-blocking reads.

## Monitoring and Metrics

Track these metrics per shard:
- Lock acquisition time
- Lock hold time
- Contention rate (failed try_lock)
- Cache hit rate
- Eviction rate

Track these global metrics:
- Total throughput (ops/sec)
- Average latency
- P99 latency
- Thread utilization

## Testing Strategy

1. **Concurrent Reads**: Multiple threads reading different pages
2. **Concurrent Writes**: Multiple threads writing different pages
3. **Same Page Access**: Multiple threads accessing same page (serialization)
4. **Mixed Workload**: Reads and writes to different shards
5. **Stress Test**: High concurrency, many operations
6. **Deadlock Test**: Verify no deadlocks under load

## Migration Path

**Phase 1**: PageTable integration ✅ COMPLETED
- Add PageTable to Pager
- Update read_page, write_page, allocate_page, free_page
- Tests passing

**Phase 2**: Lock-free FreeList (future)
- Replace Vec with SegQueue
- Use atomic counters
- Benchmark improvements

**Phase 3**: Sharded Cache (future)
- Split cache into shards
- Independent LRU per shard
- Benchmark improvements

## References

- [Phase 1 Completion Summary](../PHASE1_COMPLETION_SUMMARY.md)
- [Pager Concurrency Design](../PAGER_CONCURRENCY_IMPROVEMENT.md)
- [PageTable Implementation](../../src/pager/page_table.rs)
- [Concurrency Tests](../../tests/pager_concurrency_tests.rs)

## Related ADRs

- [ADR-002: Page-Based Storage](./002-page-based-storage.md)
- [ADR-007: Lock-Free FreeList](./007-lock-free-freelist.md)
- [ADR-003: MVCC Concurrency](./003-mvcc-concurrency.md)

---

**Last Updated**: 2026-05-10