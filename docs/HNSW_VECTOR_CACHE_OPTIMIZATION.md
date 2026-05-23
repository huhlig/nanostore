# HNSW Vector Cache Optimization

**Issue**: nanokv-qc1j - Implement optimized RobustPrune diversity heuristic for HNSW

**Date**: 2026-05-23

## Summary

Implemented a vector-only cache architecture to optimize HNSW neighbor selection performance. The goal is to enable RobustPrune diversity heuristic while maintaining <60s insertion time for 15K vectors and achieving >80% cluster purity.

## Changes Implemented

### 1. VectorCache Structure

Added a lightweight vector-only cache separate from the node cache:

```rust
struct VectorCache {
    vectors: HashMap<NodeId, Vec<f32>>,
    capacity: usize,
    stats: CacheStats,
}
```

**Key Design Decisions:**
- Stores only `Vec<f32>` instead of full `HnswNode` (10-100x smaller memory footprint)
- No LRU tracking to avoid lock contention on reads
- Simple eviction policy (remove arbitrary entry when at capacity)
- Separate from node cache to allow different eviction policies

### 2. Cache Integration

Updated node storage operations to populate both caches:

- `load_node()`: Populates both node cache and vector cache
- `store_node()`: Stores in both caches
- `update_node()`: Updates both caches

This ensures vectors are always available in the vector cache when nodes are accessed.

### 3. Optimized select_neighbors()

Modified to use vector cache for fast distance calculations:

```rust
// Try vector cache first (fast path)
{
    let mut cache = self.vector_cache.write().unwrap();
    for candidate in &prune_pool {
        if let Some(vector) = cache.get(candidate.node_id) {
            candidate_vectors.insert(candidate.node_id, vector.clone());
        } else {
            missing_nodes.push(candidate.node_id);
        }
    }
}

// Load missing vectors (slow path)
for node_id in missing_nodes {
    if let Ok(node) = self.load_node(node_id) {
        candidate_vectors.insert(node_id, node.vector);
    }
}
```

**Benefits:**
- Cache hits avoid expensive node deserialization
- Batch loading of missing vectors
- Amortizes I/O cost across all diversity checks

### 4. Optimized prune_connections()

Updated to use vector cache for distance calculations while maintaining greedy selection:

```rust
// Use vector cache for fast distance calculations
let mut missing_nodes = Vec::new();
let mut neighbor_vectors = HashMap::new();

// Try vector cache first (fast path)
{
    let mut cache = self.vector_cache.write().unwrap();
    for &neighbor_id in &old_neighbors {
        if let Some(vector) = cache.get(neighbor_id) {
            neighbor_vectors.insert(neighbor_id, vector.clone());
        } else {
            missing_nodes.push(neighbor_id);
        }
    }
}
```

**Important Note:** Currently uses greedy k-NN selection, not RobustPrune. This is because:
1. `prune_connections` is called O(N) times during insertion
2. RobustPrune's O(M²) complexity makes it too expensive here
3. The diversity heuristic in `select_neighbors` (during initial selection) provides the primary benefit

### 5. Cache Statistics

Added vector cache statistics to the `verify()` method:

```rust
// Add vector cache statistics
let (vec_hit_rate, vec_hits, vec_misses, vec_evictions) = {
    let cache = self.vector_cache.read().unwrap();
    let stats = cache.stats();
    (stats.hit_rate() * 100.0, stats.hits, stats.misses, stats.evictions)
};
```

## Performance Characteristics

### Expected Improvements

1. **Vector Cache Hit Rate**: Should be >90% during insertion due to locality
2. **Distance Calculation Speed**: 10-100x faster for cached vectors (no deserialization)
3. **Memory Overhead**: ~20MB for 20K cached 128-dim vectors (vs ~120MB for full nodes)

### Current Status

- **Implementation**: Complete
- **Testing**: In progress
- **Performance Target**: <60s for 15K vectors (not yet verified)
- **Quality Target**: >80% cluster purity (not yet verified)

## Next Steps

### Immediate

1. **Add Performance Instrumentation**: Add timing measurements to identify bottlenecks
   - Measure time in search_layer, select_neighbors, connect_nodes, prune_connections
   - Log every 100 insertions to track performance degradation

2. **Profile Test Execution**: Run test with profiling to identify actual bottlenecks
   - May not be in neighbor selection at all
   - Could be in search_layer, node I/O, or other operations

3. **Optimize Based on Data**: Once we know where time is spent, optimize accordingly

### Future Optimizations

1. **SIMD Distance Calculations**: Use AVX2/AVX-512 for batch distance computations
   - 4-8x speedup possible
   - Requires unsafe code and platform-specific features

2. **GPU Acceleration**: Offload distance calculations to GPU for very large graphs
   - 10-100x speedup possible
   - Requires CUDA/OpenCL integration
   - Only beneficial for large-scale deployments

3. **Approximate Diversity**: Trade accuracy for speed
   - Sample subset of selected neighbors for diversity check
   - Use distance bounds instead of exact distances
   - Probabilistic diversity checks

4. **Batch Operations**: Group multiple insertions and process together
   - Amortize overhead across multiple vectors
   - Better cache utilization
   - More complex API

## Lessons Learned

1. **Measure Before Optimizing**: Don't assume where the bottleneck is - profile first
2. **Cache Hierarchy**: Separate caches for different data types can improve hit rates
3. **Algorithmic Complexity**: O(M²) operations need careful placement in hot paths
4. **Incremental Approach**: Better to ship working code than perfect code that's too slow

## References

- HNSW Paper: "Efficient and robust approximate nearest neighbor search using Hierarchical Navigable Small World graphs"
- Algorithm 4: RobustPrune (page 7)
- Issue nanokv-3td0: Original cluster quality issue
- Issue nanokv-l2vv: Node cache implementation
- Issue nanokv-qc1j: This optimization work
- docs/HNSW_ROBUSTPRUNE_IMPLEMENTATION_ATTEMPT.md: Previous attempt analysis
- docs/HNSW_PERFORMANCE_ISSUE_SUMMARY.md: Performance analysis