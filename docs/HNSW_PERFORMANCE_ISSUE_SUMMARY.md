# HNSW Performance Issue Summary

**Issue**: nanokv-3td0 - HNSW cluster search quality and performance

**Date**: 2026-05-23

## Problem Statement

HNSW neighbor selection during graph construction is extremely slow, with test_clustered_vectors_15k taking >2100 seconds (35+ minutes) and still running.

## Root Cause

**Vector Loading Strategy**: The implementation uses on-demand loading of node vectors during diversity pruning, causing severe performance degradation.

### Why It's Slow

1. **Diversity Pruning Algorithm** (HNSW paper Algorithm 4):
   - Examines candidates in distance order
   - For each candidate, checks distance to all already-selected neighbors
   - Requires loading candidate vector for distance calculations

2. **On-Demand Loading Overhead**:
   - Each `load_node()` call has overhead: lock acquisition, cache lookup, deserialization
   - Even with node cache, repeated calls are expensive
   - For bounded pool of 48 candidates, potentially 48 load operations
   - Each load operation repeated across 15K insertions

3. **Complexity**:
   - Bounded pool: 3×M = 48 candidates (M=16)
   - Distance calculations: O((3M)²) = ~2304 per insertion
   - Total insertions: 15K
   - Total operations: ~34.5M distance calculations
   - With on-demand loading overhead: unacceptable performance

## Current Implementation

```rust
// Bounded diversity pruning with on-demand loading
for candidate in prune_pool {
    let candidate_vector = load_node(candidate.node_id).vector;  // SLOW!
    
    for selected_vector in &selected_vectors {
        let dist = distance(candidate_vector, selected_vector);
        // diversity check...
    }
}
```

**Performance**: >2100s and still running

## Proposed Solution

**Pre-load vectors for bounded pool**:

```rust
// Pre-load all vectors in bounded pool upfront
let mut candidate_vectors = HashMap::new();
for candidate in &prune_pool {
    candidate_vectors.insert(candidate.id, load_node(candidate.id).vector);
}

// Then use pre-loaded vectors for diversity checks
for candidate in prune_pool {
    let candidate_vector = candidate_vectors.get(&candidate.id);
    // diversity check with no additional I/O...
}
```

**Benefits**:
- Each vector loaded exactly once (48 loads instead of potentially 48×M loads)
- Amortizes I/O cost across all diversity checks
- HashMap lookup is O(1) and very fast
- No lock contention during diversity checks

**Trade-offs**:
- Loads all 48 vectors even though we only select 16
- Uses more memory temporarily (48 vectors vs M vectors)
- But: acceptable trade-off for 100× performance improvement

## Questions for Investigation

Need to examine production HNSW implementations (hnswlib, Faiss) to understand:

1. **Vector Storage**: Do they keep vectors in memory with the graph structure?
2. **Loading Strategy**: Do they pre-load candidate vectors or use on-demand loading?
3. **Caching**: Do they use a separate lightweight vector cache?
4. **Data Structure**: Is there a smarter organization that avoids repeated loads?
5. **Batching**: Do they batch vector loads somehow?

## Current Status

- **Code**: Pre-loading fix implemented and pushed
- **Test**: Still running (>2100s), waiting for completion
- **Next**: Need test results to verify performance improvement
- **Future**: May need to examine other implementations for further optimization

## Related Issues

- nanokv-3td0: Original cluster quality issue
- nanokv-l2vv: Node cache implementation (completed)
- nanokv-qc1j: Future full RobustPrune optimization (deferred)