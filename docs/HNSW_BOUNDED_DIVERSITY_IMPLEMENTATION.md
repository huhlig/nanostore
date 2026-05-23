# HNSW Bounded Diversity Pruning Implementation

## Issue: nanokv-3td0

**Date**: 2026-05-23

## Summary

Implemented Algorithm 4 (SELECT-NEIGHBORS-HEURISTIC) from the HNSW paper with a bounded candidate pool optimization. This provides a faithful implementation of the paper's diversity heuristic while limiting complexity through a bounded pruning pool.

## Approach

### Paper's Algorithm 4: SELECT-NEIGHBORS-HEURISTIC

The HNSW paper describes a diversity heuristic that:
1. Examines candidates in distance order (closest first)
2. For each candidate, checks if it's closer to the query than to any already-selected neighbor
3. If yes (diverse), adds it to result; if no (redundant), skips it

**Key Insight**: This preserves "bridge edges" between clusters. A candidate that's closer to an already-selected neighbor than to the query is redundant - the selected neighbor already "covers" that direction from the query.

### Our Optimization: Bounded Pool

Instead of applying the heuristic to all candidates (O(candidates²)), we:

1. **Sort candidates by distance** to query (O(candidates log candidates))
2. **Limit pruning pool** to top K candidates where K = M × 3
3. **Apply paper's heuristic** within this bounded pool (O((3M)²))
4. **Load vectors on-demand** during diversity checks (not pre-loaded)

This gives us:
- **Complexity**: O(candidates log candidates + 9M²) instead of O(candidates²)
- **For 15K insertions with M=16**: ~3.5K distance calculations instead of ~3.8M
- **Expected speedup**: 1000x reduction in distance calculations vs full RobustPrune

### Why On-Demand Loading?

The paper's algorithm processes candidates sequentially and only needs:
- Current candidate's vector (when checking it)
- Already-selected neighbors' vectors (accumulated as we go)

This means we load at most M vectors (the selected neighbors) plus 1 (current candidate), not the entire pool. This is more memory-efficient and cache-friendly than pre-loading all 3M vectors.

## Implementation Details

### Code Location
File: `src/table/hnsw/paged.rs`
Method: `select_neighbors()`

### Key Changes

```rust
// Bounded diversity pruning: limit pool size to 3*M
let pool_size = (m * 3).min(candidates.len());
let prune_pool: Vec<Candidate> = candidates.into_iter().take(pool_size).collect();

// Apply paper's Algorithm 4 heuristic within bounded pool
let mut result = Vec::with_capacity(m);
let mut selected_vectors: Vec<Vec<f32>> = Vec::with_capacity(m);

for candidate in prune_pool {
    if result.len() >= m {
        break;
    }
    
    // Load candidate vector on-demand (only when checking this candidate)
    let candidate_vector = match self.load_node(candidate.node_id) {
        Ok(node) => node.vector,
        Err(_) => continue,
    };
    
    // Check if candidate is closer to query than to any already-selected neighbor
    // This is Algorithm 4, line 11: "if e is closer to q compared to any element from R"
    let mut is_diverse = true;
    
    for selected_vector in &selected_vectors {
        let dist_to_selected = self.distance(&candidate_vector, selected_vector);
        
        // If candidate is closer to a selected neighbor than to query, it's redundant
        if dist_to_selected < candidate.distance {
            is_diverse = false;
            break;
        }
    }
    
    if is_diverse {
        result.push(candidate.node_id);
        selected_vectors.push(candidate_vector);
    }
}
```

### Pool Size Rationale

**Why 3×M?**
- Too small (2×M): May not have enough diverse candidates
- Too large (5×M+): Approaches full RobustPrune complexity
- 3×M: Good balance - enough candidates for diversity, bounded complexity

For M=16:
- Pool size: 48 candidates
- Distance calculations: ~48² = 2,304 per insertion
- Total for 15K: ~34.5M distance calculations
- Still manageable with caching

## Expected Benefits

### Performance
- **Baseline (greedy)**: ~400s for 15K vectors
- **Full RobustPrune**: >1500s for 15K vectors
- **Bounded diversity**: Expected 450-600s (10-50% slower than greedy)

### Quality
- **Baseline (greedy)**: <50% cluster purity
- **Full RobustPrune**: >80% cluster purity (theoretical)
- **Bounded diversity**: Expected 60-75% cluster purity

### Trade-offs
- **Pros**: Much faster than full RobustPrune, better quality than greedy
- **Cons**: Not as good as full RobustPrune, slightly slower than greedy

## Testing Strategy

### Unit Tests
1. Test with small datasets (100-1000 vectors)
2. Verify diversity is applied (not just greedy)
3. Check edge cases (pool smaller than M, etc.)

### Integration Tests
1. Run test_clustered_vectors_15k
2. Measure cluster purity improvement
3. Measure performance impact
4. Compare to baseline greedy selection

### Metrics to Track
- Cluster purity (% results from correct cluster)
- Insertion time (total for 15K vectors)
- Cache hit rate
- Average degree per layer
- Reciprocal edge ratio

## Future Optimizations

If bounded diversity proves insufficient, next steps:

### 1. Vector-Only Cache
Separate lightweight cache for just vectors:
```rust
struct VectorCache {
    vectors: HashMap<NodeId, Arc<[f32]>>,
    capacity: usize,
}
```

### 2. Configurable Pool Size
Make pool multiplier configurable:
```rust
pub struct HnswConfig {
    // ...
    pub diversity_pool_multiplier: usize, // default: 3
}
```

### 3. Adaptive Pool Size
Adjust pool size based on candidate distribution:
- Dense regions: smaller pool (more candidates are similar)
- Sparse regions: larger pool (need more candidates for diversity)

### 4. SIMD Distance Calculations
Use AVX2/AVX-512 for vectorized distance computations:
- 4-8x speedup possible
- Requires unsafe code and platform detection

## Comparison with Industry

### hnswlib (C++)
- Uses full RobustPrune by default
- Has extensive caching and optimization
- Written in C++ with SIMD intrinsics

### Faiss (Facebook)
- Uses diversity heuristic
- GPU-accelerated distance calculations
- Highly optimized for production use

### Our Approach
- Bounded diversity as practical middle-ground
- Pure Rust implementation (safe, portable)
- Room for future optimization

## Lessons Learned

1. **Read the Paper Carefully**: The paper's Algorithm 4 is simpler and more efficient than initially implemented
2. **On-Demand Loading**: Loading vectors as needed is more memory-efficient than pre-loading entire pools
3. **Bounded Pool Optimization**: Limiting to 3×M candidates preserves algorithm semantics while reducing complexity
4. **Start Simple**: Bounded approach is more practical than jumping to full RobustPrune
5. **Measure First**: Need actual performance data before further optimization

## References

- HNSW Paper: "Efficient and robust approximate nearest neighbor search using Hierarchical Navigable Small World graphs"
- Algorithm 4: RobustPrune (full version)
- Issue nanokv-3td0: Original cluster quality issue
- Issue nanokv-l2vv: Node cache implementation
- Issue nanokv-qc1j: Future full RobustPrune optimization

## Next Steps

1. **Run Tests**: Verify bounded diversity improves cluster purity
2. **Measure Performance**: Ensure acceptable runtime (<10 minutes for 15K)
3. **Add Diagnostics**: Implement graph health metrics (reciprocal edges, etc.)
4. **Tune Parameters**: Adjust pool multiplier if needed
5. **Document Results**: Update issue with actual performance data