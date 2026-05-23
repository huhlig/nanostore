# HNSW to JVector Migration Plan

**Date**: 2026-05-23  
**Status**: Planning Phase  
**Decision**: Evolutionary approach - fix HNSW first, then evolve to JVector architecture

---

## Executive Summary

We are adopting a **two-phase evolutionary approach** to transform our HNSW implementation into a JVector/DiskANN-inspired hybrid architecture:

- **Phase 1 (P1)**: Fix immediate performance issues by addressing storage abstraction impedance
- **Phase 2 (P2)**: Evolve toward full JVector architecture for larger-than-memory datasets

This approach balances risk, learning, and delivery speed while building toward the optimal long-term architecture.

---

## Background

### Current Problem

Our paged HNSW implementation has a fundamental architectural mismatch:
- **HNSW assumes**: Cheap vector access (pointer arithmetic, direct memory)
- **Our implementation**: Expensive vector access (node cache → lock → page → deserialize → extract)

This causes catastrophic performance: >2100s for 15K vector insertion (should be <60s).

### Why Not Immediate Full Rewrite?

1. **Complexity**: JVector requires PQ infrastructure, SIMD kernels, tier-aware paging, concurrent construction
2. **Scale mismatch**: JVector benefits shine at millions of vectors; we're testing with 15K
3. **Learning opportunity**: Fix the abstraction boundary first to understand real bottlenecks
4. **Risk management**: Incremental approach allows pivoting if assumptions are wrong

### Strategic Direction

**Agreed**: JVector/DiskANN architecture is the right long-term destination for paged vector search.

**Approach**: Start with evolutionary fixes, validate assumptions, then commit to full architecture.

---

## Phase 1: Fix HNSW Performance (Priority 1)

**Epic**: `nanokv-wzsv` - Fix HNSW Performance with VectorAccessor Architecture

**Timeline**: 6-10 weeks  
**Goal**: Achieve <60s for 15K vectors, >80% cluster purity  
**Risk**: Low - incremental improvements to existing code

### Issues

#### 1. Design and implement VectorAccessor trait (`nanokv-h2qw`)
**Type**: Feature | **Priority**: P1 | **Estimate**: 1-2 weeks

Create abstraction layer separating vector access from graph node loading.

**Requirements**:
- Define `VectorAccessor` trait with methods: `dim()`, `prefetch(ids)`, `get(id) -> VectorRef`, `distance_between(a, b)`
- Implement for current paged storage
- Support batch loading of multiple vectors
- Use zero-copy or minimal-copy access patterns
- Design for future quantized vector support

**Success Criteria**:
- Trait compiles and passes basic tests
- Can load vectors without deserializing full nodes
- Foundation for all subsequent improvements

---

#### 2. Implement batch vector loading with PruneScratch arena (`nanokv-a4dy`)
**Type**: Feature | **Priority**: P1 | **Estimate**: 1 week | **Depends**: nanokv-h2qw

Replace on-demand loading with batch preload using reusable scratch buffers.

**Requirements**:
- Create `PruneScratch` struct with `SmallVec` for ids/vectors/distances (capacity 64)
- Implement `load_vectors_batch()` that loads all candidate vectors once
- Use scratch arena to avoid per-insertion heap allocation
- Replace HashMap with linear array for 3M=48 candidates
- Benchmark to verify 10-100x improvement

**Success Criteria**:
- Insertion time drops from >2100s to <200s for 15K vectors
- Memory allocation per insertion is minimal
- Batch loading is measurably faster than on-demand

---

#### 3. Refactor HNSW neighbor selection to use VectorAccessor (`nanokv-fzvh`)
**Type**: Feature | **Priority**: P1 | **Estimate**: 1 week | **Depends**: nanokv-a4dy

Update HNSW insertion and pruning to use VectorAccessor instead of load_node().

**Requirements**:
- Modify `select_neighbors()` to accept VectorAccessor parameter
- Remove all `load_node()` calls from pruning hot path
- Use batch prefetch before pruning loop
- Update all distance calculations to use VectorAccessor methods
- Ensure no graph node deserialization during pairwise distance checks

**Success Criteria**:
- No `load_node()` calls in pruning code
- Distance calculations use VectorAccessor only
- Performance matches or exceeds batch loading alone

---

#### 4. Implement Vamana RobustPrune as optional pruning strategy (`nanokv-onm6`)
**Type**: Feature | **Priority**: P1 | **Estimate**: 2-3 weeks | **Depends**: nanokv-fzvh

Add Vamana-style diversity pruning as alternative to current HNSW heuristic.

**Requirements**:
- Create `NeighborSelectionStrategy` enum (GreedyNearest, HnswHeuristic, VamanaRobustPrune)
- Implement RobustPrune algorithm from DiskANN paper
- Add alpha parameter for candidate pool size control
- Make strategy configurable in HnswConfig
- Benchmark graph quality (recall@k, cluster purity) vs construction time

**Success Criteria**:
- >80% cluster purity in test_clustered_vectors_15k
- <60s construction time for 15K vectors
- Configurable strategy selection works correctly

---

#### 5. Add comprehensive HNSW performance and quality benchmarks (`nanokv-lean`)
**Type**: Task | **Priority**: P1 | **Estimate**: 1 week | **Can run in parallel**

Create benchmarks to validate P1 improvements and guide P2 decisions.

**Requirements**:
- Benchmark insertion throughput (vectors/sec) for 10K, 50K, 100K datasets
- Measure search recall@k and latency
- Test cluster purity with different pruning strategies
- Profile memory usage and cache hit rates
- Compare VectorAccessor vs old load_node() approach
- Benchmark different vector dimensions (128, 384, 768, 1536)

**Success Criteria**:
- Criterion benchmarks run reliably
- Clear performance comparison between approaches
- Data to inform P2 decisions

---

#### 6. Implement optimized RobustPrune diversity heuristic (`nanokv-qc1j`)
**Type**: Feature | **Priority**: P2 | **Note**: Moved from original issue, now part of P1 epic

This is the original issue that started this analysis. It's now superseded by the more structured approach above but remains linked to the P1 epic.

---

### Phase 1 Success Criteria

- ✅ <60s insertion time for 15K vectors (currently >2100s)
- ✅ >80% cluster purity (currently <50%)
- ✅ No `load_node()` calls in pruning hot path
- ✅ Batch vector loading implemented and benchmarked
- ✅ Vamana RobustPrune available as optional strategy
- ✅ Comprehensive benchmarks validate improvements

### Phase 1 Decision Point

After completing P1, evaluate:
1. Did VectorAccessor give 10x+ improvement? (If yes, maybe JVector is overkill)
2. Is performance acceptable for target scale? (If yes, maybe stop here)
3. Do we need larger-than-memory support? (If no, maybe defer P2)
4. Is quantization complexity justified? (Prototype to validate)

**Only proceed to P2 if**:
- P1 validates the approach
- We need to scale beyond current performance
- We have capacity for research-level work

---

## Phase 2: Evolve to JVector Architecture (Priority 2)

**Epic**: `nanokv-r0dl` - Evolve to JVector/DiskANN Hybrid Architecture

**Timeline**: 16-26 weeks (4-6 months)  
**Goal**: Support billion-scale vector search with limited RAM  
**Risk**: High - research-level complexity, many unknowns

### Issues

#### 1. Design and implement product quantization (PQ) infrastructure (`nanokv-0ys2`)
**Type**: Feature | **Priority**: P2 | **Estimate**: 4-6 weeks | **Blocks**: P1 epic completion

Add compressed vector scoring tier for larger-than-memory datasets.

**Requirements**:
- Implement PQ training (k-means clustering on subvectors)
- Create `PqCodeStore` for compressed vectors in memory
- Implement approximate distance computation using PQ codes
- Add codebook serialization/deserialization
- Support multiple quantization methods (PQ, scalar quantization, binary quantization)
- Benchmark accuracy vs compression ratio

**Success Criteria**:
- PQ training produces valid codebooks
- Approximate distances correlate with full-precision
- Compression ratio meets targets (8-16x)
- Accuracy degradation is acceptable (<5% recall loss)

**Risks**:
- PQ training is complex (k-means, subvector splitting)
- Rust implementation may differ from Python/Java references
- Accuracy/compression tradeoff may not be acceptable

---

#### 2. Implement SIMD distance computation kernels (`nanokv-7ogt`)
**Type**: Feature | **Priority**: P2 | **Estimate**: 2-3 weeks | **Can run in parallel**

Add SIMD-accelerated distance functions for hot path performance.

**Requirements**:
- Implement SIMD kernels for L2, cosine, dot product distances
- Support AVX2, AVX-512, NEON instruction sets
- Add runtime CPU feature detection
- Provide scalar fallback for unsupported platforms
- Integrate with VectorAccessor trait
- Benchmark throughput (distances/sec) vs scalar baseline

**Success Criteria**:
- 4-8x speedup for distance computation
- Works on x86-64 and ARM64
- Graceful fallback on unsupported CPUs

---

#### 3. Implement two-pass search with approximate-then-rerank (`nanokv-6mwe`)
**Type**: Feature | **Priority**: P2 | **Estimate**: 3-4 weeks | **Depends**: nanokv-0ys2

Add JVector-style two-pass search for accuracy/performance tradeoff.

**Requirements**:
- First pass: use PQ approximate distances for graph traversal
- Second pass: rerank top-k candidates with full-precision vectors
- Add search configuration (ef_search, rerank_factor)
- Support approximate-only mode for speed
- Benchmark recall@k vs latency tradeoff

**Success Criteria**:
- Two-pass search maintains >95% recall
- Latency improvement over full-precision search
- Configurable accuracy/speed tradeoff

---

#### 4. Implement tier-aware pager with memory-resident upper layers (`nanokv-g5pb`)
**Type**: Feature | **Priority**: P2 | **Estimate**: 3-4 weeks

Extend pager to support JVector-style tiered storage.

**Requirements**:
- Add page tier metadata (hot/warm/cold)
- Pin upper graph layers in memory (never evict)
- Implement tier-aware cache eviction policy
- Add mmap support for read-only vector data
- Support async prefetch for lower layers
- Add metrics for tier hit rates

**Success Criteria**:
- Upper layers stay resident under memory pressure
- Lower layers can be evicted without breaking search
- Mmap reduces memory footprint for large datasets

**Risks**:
- Significant pager changes may destabilize other code
- Tier policy may be hard to tune
- Mmap on Windows has different semantics

---

#### 5. Design MVCC transaction semantics for graph operations (`nanokv-8gkv`)
**Type**: Task | **Priority**: P2 | **Estimate**: 2-3 weeks | **Research**

Research and design how MVCC transactions work with graph modifications.

**Requirements**:
- Define transaction scope for graph updates (local vs distant node modifications)
- Design rollback mechanism for graph structure changes
- Handle concurrent insertions with optimistic concurrency
- Define isolation semantics for graph traversal
- Document design decisions and tradeoffs

**Success Criteria**:
- Written design document with clear semantics
- Identified failure modes and mitigation strategies
- Consensus on approach before implementation

**Risks**:
- This is research-level work with no clear solution
- May discover fundamental incompatibilities
- Could block concurrent construction work

---

#### 6. Implement concurrent graph construction with lock-free techniques (`nanokv-l4qt`)
**Type**: Feature | **Priority**: P2 | **Estimate**: 4-6 weeks | **Depends**: nanokv-8gkv

Add support for concurrent vector insertions following JVector's approach.

**Requirements**:
- Design lock-free or fine-grained locking for graph updates
- Handle concurrent neighbor list modifications
- Implement optimistic concurrency control for pruning
- Add conflict detection and retry logic
- Benchmark insertion throughput with multiple threads

**Success Criteria**:
- Multiple threads can insert concurrently without corruption
- Throughput scales with thread count (at least 2x with 4 threads)
- No deadlocks or livelocks under stress testing

**Risks**:
- Lock-free programming is extremely difficult in Rust
- Without GC, memory reclamation is complex
- May need to use coarser-grained locking than JVector

---

### Phase 2 Success Criteria

- ✅ PQ infrastructure working with acceptable accuracy
- ✅ Two-pass search implemented and benchmarked
- ✅ Tier-aware paging keeps upper layers hot
- ✅ SIMD distance kernels provide measurable speedup
- ✅ Transaction semantics designed and documented
- ✅ Concurrent construction works correctly
- ✅ Can handle 1M+ vectors with limited RAM

### Phase 2 Decision Points

**After PQ implementation**:
- Is accuracy acceptable? (If no, reconsider quantization approach)
- Is complexity justified? (If no, maybe full-precision is sufficient)

**After tier-aware paging**:
- Does it actually improve performance? (Measure carefully)
- Is the pager stable? (Watch for regressions)

**After transaction design**:
- Is the design implementable? (May need to simplify)
- Does it fit Rust's ownership model? (Critical question)

---

## Risk Management

### High-Risk Areas

1. **Product Quantization**
   - Risk: Complex to implement correctly
   - Mitigation: Prototype early, validate against reference implementations
   - Fallback: Use simpler scalar quantization

2. **MVCC Transaction Semantics**
   - Risk: May be fundamentally incompatible with graph operations
   - Mitigation: Design phase before implementation
   - Fallback: Coarser-grained locking, accept limitations

3. **Concurrent Construction**
   - Risk: Lock-free programming is extremely difficult in Rust
   - Mitigation: Start with fine-grained locking, optimize later
   - Fallback: Single-writer model with read-only search

4. **Tier-Aware Paging**
   - Risk: May destabilize existing pager
   - Mitigation: Extensive testing, feature flag
   - Fallback: Keep current pager, use external vector cache

### Mitigation Strategies

- **Incremental delivery**: Each issue is independently valuable
- **Feature flags**: New features can be disabled if problematic
- **Comprehensive testing**: Stress tests, property tests, benchmarks
- **Documentation**: Design docs before implementation
- **Prototyping**: Validate hardest parts early

---

## Success Metrics

### Phase 1 Metrics

| Metric | Current | Target | Measurement |
|--------|---------|--------|-------------|
| Insertion time (15K vectors) | >2100s | <60s | test_clustered_vectors_15k |
| Cluster purity | <50% | >80% | Same test |
| Memory per insertion | High (HashMap) | Low (SmallVec) | Profiling |
| Cache hit rate | Unknown | >90% | Metrics |

### Phase 2 Metrics

| Metric | Target | Measurement |
|--------|--------|-------------|
| Dataset size | 1M+ vectors | Stress tests |
| Memory footprint | <4GB for 1M vectors | Profiling |
| Search recall@10 | >95% | Benchmark suite |
| Search latency | <10ms p99 | Benchmark suite |
| Insertion throughput | >1000 vectors/sec | Benchmark suite |
| Concurrent throughput | 2x with 4 threads | Concurrent benchmark |

---

## Timeline

### Phase 1: 6-10 weeks
- Week 1-2: VectorAccessor trait
- Week 3: Batch loading + scratch arena
- Week 4: Refactor neighbor selection
- Week 5-7: Vamana RobustPrune
- Week 6-10: Benchmarks (parallel)

### Phase 2: 16-26 weeks
- Week 1-6: PQ infrastructure
- Week 1-3: SIMD kernels (parallel)
- Week 7-10: Two-pass search
- Week 11-14: Tier-aware paging
- Week 15-17: Transaction design
- Week 18-23: Concurrent construction

**Total**: 22-36 weeks (5.5-9 months)

---

## Open Questions

### Phase 1
1. What's the actual bottleneck after VectorAccessor? (Profile to find out)
2. Is Vamana RobustPrune worth the complexity? (Benchmark to decide)
3. Can we get acceptable performance without quantization? (P1 will answer)

### Phase 2
1. What quantization method works best for our use case? (PQ vs scalar vs binary)
2. Can MVCC transactions work with graph operations? (Design phase will answer)
3. Is lock-free construction achievable in Rust? (May need to compromise)
4. What's the right tier policy for our workload? (Needs experimentation)

---

## References

- **Owl Analysis**: `docs/HNSW_TO_JVECTOR_OWL_ANALYSIS.md`
- **Comparison Document**: `docs/ann_neighbor_selection_comparison.md`
- **Performance Summary**: `docs/HNSW_PERFORMANCE_ISSUE_SUMMARY.md`
- **JVector Repository**: https://github.com/datastax/jvector
- **DiskANN Paper**: https://suhasjs.github.io/files/diskann_neurips19.pdf
- **HNSW Paper**: https://arxiv.org/abs/1603.09320

---

## Conclusion

This migration plan balances **pragmatism** (fix immediate issues) with **vision** (evolve to optimal architecture). By starting with P1, we:

1. **Validate assumptions** about the real bottlenecks
2. **Learn** about our specific constraints and workload
3. **Deliver value** quickly (working HNSW in weeks, not months)
4. **Reduce risk** by making incremental, testable changes
5. **Preserve optionality** to pivot if P2 proves too complex

The evolutionary approach respects the complexity of the problem while maintaining forward momentum. We're not just copying JVector—we're building a **transactional, paged, Rust-native vector database** that borrows the best ideas from JVector/DiskANN while fitting our unique constraints.

**Next Steps**:
1. Review and approve this plan
2. Start with `nanokv-h2qw` (VectorAccessor trait)
3. Measure, learn, adapt
4. Decide on P2 after P1 completes