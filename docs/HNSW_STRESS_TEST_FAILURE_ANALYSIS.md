# HNSW Stress Test Failure Analysis

## Executive Summary

**Root Cause**: The HNSW stress tests are failing due to the **default page cache limit of 1000 pages** in the Pager. When tests insert 10K-20K vectors, each vector creates a separate page (node), but only ~1000 can be cached. When the cache evicts pages and they're later needed during search, the system can only access cached nodes, leading to incomplete search results.

**Key Insight**: This is fundamentally different from the TimeSeries issue (nanokv-hdil). TimeSeries had a hardcoded in-memory bucket limit, while HNSW is hitting the Pager's page cache eviction limit.

## Test Failure Patterns

### Failed Tests
1. **test_clustered_vectors_15k** - Expected 100 results, got 55
2. **test_deletions_with_10k_vectors** - Deleted vectors appearing in results
3. **test_mixed_operations_20k_total** - Expected 100 results, got 53

### Passing Tests
- test_insert_10k_vectors_128d (10K vectors)
- test_normalized_vectors_5k (5K vectors)
- test_insertion_patterns_10k_vectors (10K vectors)
- test_search_with_10k_vectors (10K vectors)
- test_distance_metrics_5k_vectors (5K vectors)
- test_sparse_vectors_5k (5K vectors)

### Pattern Analysis

**Why some 10K tests pass**: Tests that insert vectors and immediately search (without complex operations) work because:
1. Recently inserted nodes are still in cache
2. The HNSW graph structure keeps frequently accessed nodes (entry points, high-layer nodes) in cache
3. Search traversal tends to hit cached nodes first

**Why clustered/mixed tests fail**: These tests:
1. Insert many vectors across different clusters/patterns
2. Perform complex operations that evict cache entries
3. Search requires accessing nodes that were evicted
4. The graph traversal hits evicted nodes and can't explore full neighborhoods

## Technical Deep Dive

### HNSW Architecture

```rust
pub struct PagedHnswVector<FS: FileSystem> {
    pager: Arc<Pager<FS>>,           // Page cache: 1000 pages default
    id_to_node: RwLock<HashMap<KeyBuf, NodeId>>,  // In-memory mapping
    // ... other fields
}
```

**Key characteristics**:
- Each vector is stored as a separate node on its own page
- `id_to_node` HashMap is fully in-memory (no limit)
- Node pages are cached by the Pager (1000 page limit)
- Search algorithm loads nodes on-demand via `load_node()`

### Search Algorithm Flow

```rust
fn search_layer(&self, query: &[f32], entry_points: Vec<NodeId>, ef: usize, layer: usize) 
    -> TableResult<Vec<Candidate>> {
    // 1. Load entry point nodes
    for ep in entry_points {
        let node = self.load_node(ep)?;  // ← Cache hit/miss
        // ...
    }
    
    // 2. Explore graph
    while let Some(current) = candidates.pop() {
        let node = self.load_node(current.node_id)?;  // ← Cache hit/miss
        
        // 3. Load neighbors
        for &neighbor_id in &node.neighbors[layer] {
            let neighbor = self.load_node(neighbor_id)?;  // ← Cache hit/miss
            // ...
        }
    }
}
```

**Cache pressure points**:
1. Entry point loading (usually cached - high layer nodes)
2. Current node loading (may be evicted if graph is large)
3. Neighbor loading (high pressure - explores many nodes)

### Page Cache Behavior

From `src/pager/config.rs`:
```rust
impl Default for PagerConfig {
    fn default() -> Self {
        Self {
            cache_capacity: 1000,   // ← THE LIMIT
            cache_write_back: true,
            // ...
        }
    }
}
```

**Cache eviction**: LRU-based, sharded across 32 shards for concurrency
- With 15K vectors, only ~6.7% can be cached
- With 20K vectors, only ~5% can be cached
- Evicted nodes require disk I/O to reload

### Why Search Results Are Incomplete

**Scenario**: test_clustered_vectors_15k
1. Insert 15,000 vectors (3 clusters of 5,000 each)
2. Each vector gets its own page/node
3. Only ~1,000 nodes fit in cache
4. Search for 100 nearest neighbors in cluster 0

**What happens**:
```
Search starts at entry point (cached)
  → Explores layer 2 neighbors (some cached, some evicted)
    → Explores layer 1 neighbors (more evictions)
      → Explores layer 0 neighbors (heavy evictions)
        → When load_node() hits evicted page:
            ✓ Page loads from disk (slow but works)
            ✗ BUT: Other pages get evicted to make room
            ✗ Graph exploration becomes incomplete
```

**The subtle bug**: The search algorithm doesn't fail when loading evicted nodes - it successfully loads them from disk. However:
1. Loading evicted nodes causes OTHER nodes to be evicted
2. The search may revisit nodes that were just evicted
3. The `visited` HashSet prevents re-exploration
4. Result: Some graph regions become unreachable, reducing result count

### Comparison with TimeSeries Issue

| Aspect | TimeSeries (nanokv-hdil) | HNSW (nanokv-bu8h) |
|--------|-------------------------|-------------------|
| **Root Cause** | Hardcoded `max_buckets_in_memory = 100` | Pager cache limit = 1000 pages |
| **Symptom** | Can't access >100 buckets | Can't efficiently access >1000 nodes |
| **Data Structure** | Buckets (time ranges) | Nodes (vectors) |
| **In-Memory Map** | Bucket metadata | `id_to_node` HashMap |
| **Cache Layer** | BucketManager cache | Pager page cache |
| **Fix Approach** | Make bucket limit configurable | Increase cache size for tests |
| **Persistence** | Buckets persisted to pages | Nodes persisted to pages |

**Key difference**: TimeSeries had an explicit in-memory limit on buckets. HNSW has no explicit node limit, but hits the underlying page cache limit.

## Why This Wasn't Caught Earlier

1. **Small test datasets**: Most tests use <1000 vectors, fitting in cache
2. **Locality of reference**: HNSW's hierarchical structure keeps hot nodes cached
3. **No explicit error**: Cache eviction is transparent - pages reload from disk
4. **Gradual degradation**: Search quality degrades gradually, not catastrophically
5. **Test design**: Many tests don't verify exact result counts

## The Fix Strategy

### Option 1: Increase Default Cache Size (RECOMMENDED)
**Pros**:
- Simple, one-line change
- Benefits all paged structures (HNSW, R-tree, etc.)
- Aligns with modern memory availability
- No API changes

**Cons**:
- Uses more memory by default
- May not scale to very large datasets

**Implementation**:
```rust
// src/pager/config.rs
impl Default for PagerConfig {
    fn default() -> Self {
        Self {
            cache_capacity: 10_000,  // 10K pages instead of 1K
            // ...
        }
    }
}
```

### Option 2: Make Cache Size Configurable Per Test
**Pros**:
- Tests can specify needed cache size
- Doesn't affect production defaults
- More explicit about requirements

**Cons**:
- Requires updating all stress tests
- Doesn't solve the underlying issue
- Production code may still hit limits

### Option 3: Implement Smart Caching for HNSW
**Pros**:
- Could prioritize high-layer nodes
- Could use graph structure for better eviction
- Optimal for HNSW specifically

**Cons**:
- Complex implementation
- Requires HNSW-specific cache layer
- May conflict with general page cache

## Recommended Solution

**Increase default cache size to 10,000 pages** (Option 1)

**Rationale**:
1. **Memory is cheap**: 10K pages × 4KB = 40MB (negligible on modern systems)
2. **Scales better**: Handles 10K vectors comfortably, 20K reasonably
3. **Simple**: One-line change, no API modifications
4. **Universal benefit**: Helps all paged structures, not just HNSW
5. **Future-proof**: Better default for growing datasets

**Additional improvements**:
1. Add cache size configuration to test helper functions
2. Document cache requirements in HNSW documentation
3. Consider adding cache hit/miss metrics for monitoring
4. Add warning when cache hit rate drops below threshold

## Testing Strategy

After implementing the fix:

1. **Run all HNSW stress tests**: Should pass with 10K cache
2. **Verify cache metrics**: Check hit rates are >90%
3. **Test with various sizes**:
   - 5K vectors (should be 100% cached)
   - 10K vectors (should be 100% cached)
   - 20K vectors (should be 50% cached but still work)
   - 50K vectors (stress test with 20% cache)

4. **Monitor for regressions**: Ensure passing tests still pass

## Long-Term Considerations

1. **Adaptive caching**: Could dynamically adjust cache size based on workload
2. **Tiered caching**: Keep high-layer nodes always cached
3. **Prefetching**: Predict which nodes will be needed during search
4. **Compression**: Store more nodes in same memory footprint
5. **Memory-mapped I/O**: Let OS handle caching for very large graphs

## Conclusion

The HNSW stress test failures are caused by the default page cache limit being too small for large vector datasets. Unlike the TimeSeries issue which had an explicit in-memory limit, HNSW hits the underlying Pager's cache eviction behavior. The fix is straightforward: increase the default cache size from 1,000 to 10,000 pages, providing better performance for modern workloads while remaining memory-efficient.