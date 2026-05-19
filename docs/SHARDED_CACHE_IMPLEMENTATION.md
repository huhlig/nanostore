# Sharded Cache Implementation

## Overview

The page cache has been refactored from a single-lock design to a sharded architecture to improve concurrency and reduce lock contention in multi-threaded workloads.

## Architecture

### Design Decisions

**Number of Shards**: 32 shards
- Power of 2 for efficient modulo via bitwise operations
- Balances concurrency benefits vs overhead
- Each shard maintains independent LRU list and lock

**Shard Selection**: Simple modulo hash
- `shard_index = page_id % NUM_SHARDS`
- Provides good distribution across shards
- Fast computation with no additional dependencies

**Capacity Distribution**: Evenly divided across shards
- Each shard gets `capacity / NUM_SHARDS` slots
- Total capacity may be slightly higher due to rounding
- Acceptable tradeoff for improved concurrency

### Key Components

#### CacheShard
Each shard is an independent cache unit with:
- Own HashMap for O(1) lookups
- Own doubly-linked LRU list
- Own RwLock for thread safety
- Own statistics tracking

#### PageCache
The main cache coordinates across shards:
- Array of 32 shards
- Routes operations to appropriate shard
- Aggregates statistics across all shards
- Maintains same public API for backward compatibility

## Benefits

### Concurrency Improvements

1. **Reduced Lock Contention**
   - Operations on different shards can proceed concurrently
   - No single bottleneck lock
   - Scales better with thread count

2. **Finer-Grained Locking**
   - Each shard locked independently
   - Shorter critical sections
   - Better CPU utilization

3. **Expected Performance Gains**
   - 20-30% throughput improvement under concurrent load
   - Linear scaling up to 32 concurrent operations
   - Minimal overhead for single-threaded workloads

### Trade-offs

1. **Approximate Global LRU**
   - Each shard maintains own LRU order
   - Global LRU ordering is approximate
   - Acceptable for cache eviction purposes

2. **Capacity Rounding**
   - Total capacity may exceed configured value by up to NUM_SHARDS-1
   - Each shard enforces its own capacity independently

3. **Statistics Aggregation**
   - Stats must be aggregated across all shards
   - Slight overhead for stats() calls
   - Still O(NUM_SHARDS) which is constant

## Implementation Details

### Shard Distribution

Pages are distributed across shards using simple modulo:

```rust
fn shard_index(&self, page_id: PageId) -> usize {
    (page_id.as_u64() as usize) % NUM_SHARDS
}
```

Tests verify good distribution:
- All shards receive pages
- No shard is overloaded
- Distribution is reasonably uniform

### Thread Safety

Each shard uses `RwLock` for thread safety:
- Multiple concurrent readers per shard
- Exclusive writer per shard
- No cross-shard locking (avoids deadlocks)

### API Compatibility

The public API remains unchanged:
- `get()`, `put()`, `mark_dirty()`, `mark_clean()` work as before
- Statistics aggregated transparently
- Existing code requires no changes

## Testing

### Unit Tests

1. **Basic Operations** - Verify get/put/remove work correctly
2. **LRU Eviction** - Test per-shard eviction behavior
3. **Dirty Tracking** - Ensure dirty flags work across shards
4. **Shard Distribution** - Verify uniform distribution
5. **Concurrent Access** - Test thread safety with multiple threads
6. **Concurrent Eviction** - Test eviction under concurrent load

### Integration Tests

All existing cache integration tests pass with sharded implementation:
- Cache hit/miss scenarios
- Write-back and write-through modes
- LRU eviction with pager
- Cache statistics
- Cache clear and sync operations

## Performance Characteristics

### Time Complexity

- `get()`: O(1) average, same as before
- `put()`: O(1) average, same as before
- `mark_dirty()`: O(1), same as before
- `mark_clean()`: O(1), same as before
- `stats()`: O(NUM_SHARDS) = O(32) = O(1) constant
- `size()`: O(NUM_SHARDS) = O(32) = O(1) constant

### Space Complexity

- Overhead: NUM_SHARDS * (RwLock + small metadata)
- Negligible compared to page data
- ~1KB additional overhead for 32 shards

### Concurrency

- Up to NUM_SHARDS concurrent operations without contention
- Linear scaling for well-distributed workloads
- Graceful degradation for skewed access patterns

## Future Improvements

1. **Adaptive Sharding**
   - Adjust shard count based on workload
   - More shards for high concurrency
   - Fewer shards for low concurrency

2. **Better Hash Function**
   - Consider more sophisticated hashing
   - Improve distribution for sequential page IDs
   - Minimize collisions

3. **Per-Shard Capacity Tuning**
   - Allow different capacities per shard
   - Adapt to access patterns
   - Balance load dynamically

4. **Lock-Free Operations**
   - Explore lock-free data structures
   - Further reduce contention
   - Improve scalability

## Related Issues

- **Nanostore-rte**: Pager: Implement sharded cache for better concurrency
- **Nanostore-z34**: Pager: Coarse-grained locking limits concurrency (parent issue)

## References

- `src/pager/cache.rs` - Implementation
- `tests/cache_integration_tests.rs` - Integration tests
- `benches/cache_benchmarks.rs` - Performance benchmarks