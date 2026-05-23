# HNSW Neighbor Selection and Disk-Aware ANN Design: Comparison with DiskANN, ScaNN, and JVector

## Executive Summary

Your current problem is not primarily the `SELECT-NEIGHBORS-HEURISTIC` itself. The core issue is that the current storage abstraction makes vector access too expensive during graph construction.

Classic HNSW implementations such as hnswlib and Faiss avoid this by keeping vectors in compact memory-resident layouts and computing candidate-to-candidate distances through direct pointer or lightweight distance-computer access. DiskANN and JVector solve the larger-than-memory version differently: they explicitly separate graph structure, compressed in-memory vector representations, and full-precision disk vectors. ScaNN takes a different path again: it is partitioning and quantization first, not graph-navigation first.

For your paged HNSW design, the key lesson is:

> Do not load full graph nodes during neighbor pruning. Split vector access from node access, keep a lightweight vector tier, and make the pruning step operate over preloaded or memory-resident vector representations.

A good target architecture is a three-tier design:

1. **Graph adjacency tier** — paged, mutable, persisted.
2. **Vector scoring tier** — compact, SIMD-friendly, optionally quantized, memory-resident or mmap-backed.
3. **Full-vector tier** — full precision, paged/mmap/disk-backed, used for construction and final reranking.

This is closer to DiskANN/JVector than to naïve on-disk HNSW.

---

## 1. Problem Restatement

During HNSW insertion, after search produces a candidate set, the algorithm must select up to `M` neighbors. The HNSW paper's heuristic, often called `SELECT-NEIGHBORS-HEURISTIC`, is a diversity pruning rule:

```text
For each candidate c in distance-to-query order:
    accept c if c is closer to the query than it is to any already-selected neighbor
    otherwise reject c as redundant
```

This intentionally avoids selecting only the nearest `M` candidates. Pure greedy nearest-neighbor selection tends to over-connect locally dense clusters and under-create cross-cluster bridge edges. Those bridge edges are critical for search quality.

Your bounded variant is reasonable:

```text
M = 16
candidate_pool = 3M = 48
pairwise checks per insertion ≈ 48² = 2,304
15,000 insertions ≈ 34.5 million pairwise checks
```

That number of distance calculations is not inherently too high. It becomes disastrous only if each distance calculation goes through expensive machinery:

- graph-node cache lookup,
- lock acquisition,
- page lookup,
- deserialization,
- heap allocation,
- metadata decoding,
- or full node materialization.

The optimization problem is therefore not only algorithmic. It is an access-path problem.

---

## 2. What Classic HNSW Implementations Do

### 2.1 hnswlib-style design

hnswlib keeps vectors in memory next to the level-0 node record. The level-0 memory block stores links, vector payload, and label. Accessing a vector by internal id is essentially pointer arithmetic. During pruning, hnswlib does not deserialize graph nodes or fetch vectors through a storage abstraction; it directly calls the distance function over resident memory.

Implication:

```text
candidate id -> pointer to vector -> distance function
```

not:

```text
candidate id -> load node -> deserialize node -> extract vector -> distance function
```

This is why the HNSW heuristic is practical in hnswlib even when it performs many pairwise comparisons.

### 2.2 Faiss-style design

Faiss separates graph traversal from vector distance computation using distance-computer abstractions. HNSW pruning and graph traversal operate on ids, but distance evaluation is routed through a lightweight distance interface that knows how to access vector storage efficiently. Faiss also has batched distance computation paths in several places.

Implication:

- The graph does not own the vector access cost.
- Distance computation is specialized.
- Vector access is designed for dense, contiguous, SIMD-friendly layouts.

### 2.3 Lesson for your design

Classic HNSW assumes vectors are cheap to access. If your vectors are paged, serialized inside graph nodes, and protected by node-cache locks, then your implementation violates a hidden assumption of the algorithm.

So the right comparison is not:

```text
preload vectors vs on-demand load_node()
```

It is:

```text
node-oriented storage vs distance-oriented vector storage
```

---

## 3. DiskANN / Vamana

## 3.1 What DiskANN changes

DiskANN is not just “HNSW on disk.” It is a disk-aware graph ANN architecture built around Vamana graphs.

The important design choices are:

1. The graph and full-precision vectors are stored on SSD.
2. Compressed vectors, typically product-quantized codes, are kept in memory.
3. Search uses cheap in-memory approximate distances to guide traversal.
4. Full-precision vectors are read from disk selectively for reranking.
5. The graph is designed to reduce random disk reads and support high recall under SSD constraints.

The DiskANN paper describes storing the graph index and full-precision vectors on disk, while caching compressed vectors in memory. It also states that Vamana uses full-precision coordinates during graph construction, while product-quantized vectors are used to support efficient query-time search.

## 3.2 DiskANN's relationship to your problem

DiskANN directly addresses the fundamental issue you are encountering: random full-vector access is expensive.

Instead of repeatedly loading full vectors during search, DiskANN uses an in-memory compressed representation for most distance computations. Full vectors are only used where their accuracy matters enough to justify the I/O.

For your insertion-time pruning problem, the DiskANN-inspired answer would be:

```text
During construction:
    use full precision if available cheaply
    otherwise use a bounded construction cache or batch-read vectors

During search:
    use compressed in-memory vectors for graph traversal
    use full precision only for rerank/final validation
```

## 3.3 DiskANN/Vamana pruning vs HNSW pruning

Vamana has a related but not identical pruning strategy. It uses graph construction and pruning rules designed around sparse graph navigation and disk efficiency. The pruning concept is still diversity-oriented: avoid redundant local edges and preserve navigability. However, DiskANN's larger architectural move is more important for your situation than the exact pruning predicate.

The lesson is:

> Disk-aware ANN systems do not let graph construction or traversal repeatedly hydrate heavyweight node objects just to compute distances.

## 3.4 Design implications for your paged HNSW

A DiskANN-inspired variant of your system would add:

```rust
struct VectorTiers {
    /// Small approximate representation, memory-resident.
    pq_codes: PqCodeStore,

    /// Optional hot full-precision vector cache.
    full_vector_cache: VectorCache,

    /// Full-precision vectors stored in pages or mmap files.
    full_vector_store: VectorPageStore,
}
```

Then the neighbor-selection heuristic becomes parameterized:

```rust
pub enum PruneDistanceMode {
    FullPrecision,
    QuantizedApprox,
    ApproxThenValidate,
}
```

For small tests like 15K vectors, full precision should be memory/cache resident. For larger data, use quantized approximate distances for most pruning decisions and full-precision validation only for the final candidate set.

---

## 4. ScaNN

## 4.1 What ScaNN does differently

ScaNN is not primarily a graph ANN algorithm. It combines search-space pruning and quantization, especially anisotropic vector quantization, to accelerate maximum inner product search and related distance functions.

The Google Research announcement emphasizes compressing dataset vectors to enable fast approximate distance computations. The ScaNN README describes it as implementing search-space pruning and quantization for MIPS, while also supporting Euclidean distance, and notes x86 AVX optimization.

The key ScaNN idea is:

```text
partition / prune the search space
score candidates cheaply with quantized representations
optionally rerank with more accurate distances
```

## 4.2 Relevance to your HNSW problem

ScaNN does not answer “how should HNSW load vectors during SELECT-NEIGHBORS-HEURISTIC?” directly, because it is not an HNSW construction algorithm.

But it provides two important design lessons:

1. **Approximate distance computation is a first-class optimization.**
   You do not always need full vector distance for every intermediate decision.

2. **Partitioning can reduce candidate pressure.**
   Instead of allowing graph search to produce a broad candidate set from all regions, a partitioning layer can narrow the relevant space before graph or rerank work begins.

## 4.3 Could ScaNN-like ideas help your implementation?

Yes, but probably not as the first fix.

For your immediate issue, the main bug is access overhead. A ScaNN-like quantized scoring tier could help after you decouple vectors from graph nodes.

Potential hybrid:

```text
HNSW graph for navigability
+ quantized vector store for cheap candidate scoring
+ full vector store for final rerank
+ optional coarse partitioning for very large collections
```

However, adding partitioning too early may complicate insertions, persistence, and deletes. Your current `M=16`, `3M=48` pruning problem should be solvable with a vector accessor/cache before introducing ScaNN-style partitioning.

## 4.4 ScaNN's main warning

ScaNN is optimized for large static or semi-static search workloads. If your database needs incremental insertion, deletion, persistence, and graph updates, graph-based designs remain more naturally aligned.

So ScaNN is best viewed as a source of techniques:

- quantization,
- SIMD scoring,
- candidate reranking,
- partition-aware search,

rather than as a direct replacement for your paged HNSW.

---

## 5. JVector

## 5.1 What JVector is

JVector explicitly merges ideas from HNSW and DiskANN. Its README describes it as borrowing the hierarchical structure from HNSW and using Vamana within each layer. It keeps upper graph layers in memory for quick navigation, stores the bottom layer adjacency on disk, and uses two-pass search: compressed vector representations in memory first, more accurate representations from disk second.

This is very close to the architecture your paged HNSW likely wants to evolve toward.

## 5.2 JVector's key architectural choices

JVector's design includes:

1. **Multi-layer graph structure**, like HNSW.
2. **Vamana-style pruning within layers**, from DiskANN.
3. **Upper layers in memory**, avoiding I/O for coarse navigation.
4. **Bottom-layer adjacency on disk**, enabling larger-than-memory indexes.
5. **Compressed vectors in memory**, including PQ/BQ/fused PQ options.
6. **Second-pass reranking**, using full-resolution vectors or more accurate quantized representations.
7. **Concurrent construction**, with nonblocking concurrency control.

This maps almost perfectly onto your concerns:

```text
Do not load full graph nodes for every vector comparison.
Use compressed/memory-resident scoring for most operations.
Use disk/full precision only where necessary.
Keep graph topology and vector scoring representations separate.
```

## 5.3 JVector and construction-time pruning

The most interesting point for your specific problem is that JVector can use two-pass searches during index construction. That means the same compressed-first/full-precision-second idea is not limited to query-time search; it can also support building larger-than-memory indexes.

For your RobustPrune implementation, this suggests:

```text
candidate generation:
    graph search using cheap vector representation

candidate pruning:
    bounded pool using cached full vectors if possible
    or approximate-first pruning plus final full-precision validation

edge persistence:
    write adjacency separately from vector storage
```

## 5.4 Why JVector is the best comparator for you

Among DiskANN, ScaNN, and JVector, JVector is the closest match to your design goals because it combines:

- graph-based incremental indexing,
- HNSW-like hierarchy,
- DiskANN/Vamana-style pruning,
- disk-backed lower layers,
- compressed in-memory vector scoring,
- and full-vector reranking.

If you are building a paged Rust ANN index, JVector is probably the most relevant design reference, even though it is Java.

---

## 6. Side-by-Side Comparison

| System | Core Index Type | Vector Access Model | Disk Awareness | Construction Style | Relevance to Your Problem |
|---|---|---|---|---|---|
| hnswlib | HNSW graph | Full vectors in memory, direct pointer access | Low | Incremental | Shows why pruning is cheap when vector access is cheap |
| Faiss HNSW | HNSW graph | DistanceComputer over compact vector storage | Medium, depending index type | Batch/static-oriented but supports variants | Shows separation of graph ids from distance computation |
| DiskANN | Vamana graph | PQ in memory, full vectors on SSD | High | Batch/build-oriented, with larger-than-memory strategies | Shows how to avoid full-vector random I/O during search |
| ScaNN | Partition + quantization | Quantized scoring and reranking | Mostly memory/static workload oriented | Static/semi-static | Useful for quantization and pruning, less direct for graph insertion |
| JVector | HNSW hierarchy + Vamana layers | Compressed vectors in memory, accurate vectors on disk | High | Incremental/concurrent | Closest architectural model for paged HNSW |
| Your current design | Paged HNSW | Vector inside node load path | Medium/high storage awareness, low distance-path optimization | Incremental | Correct direction, but vector access path is too heavyweight |

---

## 7. Evaluation of Your Two Approaches

## 7.1 Approach 1: preload all vectors in bounded pool

```rust
let mut vectors = HashMap::new();
for candidate in pool {
    vectors.insert(candidate.id, load_node(candidate.id).vector);
}
```

This is better than on-demand loading, but still flawed if `load_node()` is expensive.

Pros:

- Each candidate vector is loaded once.
- Bounded to `3M`, which is only 48 for `M=16`.
- Simple and likely much faster than repeated on-demand loading.

Cons:

- Still routes through graph-node loading.
- Still pays deserialization and locking overhead.
- Uses `HashMap` where a small array/scratch buffer would be faster.
- Loads graph node metadata you do not need.

Better version:

```rust
let mut scratch: SmallVec<[(NodeId, VectorScratch); 64]> = SmallVec::new();
vector_store.load_many_into(pool.ids(), &mut scratch);
```

Avoid `HashMap` unless you truly need random lookup. With only 48 candidates, a compact vector indexed by candidate position or a tiny id-to-slot map is usually better.

## 7.2 Approach 2: load vectors on demand

```rust
for candidate in pool {
    let vec = load_node(candidate.id).vector;
}
```

This is the wrong abstraction boundary. Your measured `>2100s` runtime is consistent with repeated heavyweight access.

Even with caching, this can be slow because each logical vector access may involve:

- lock acquisition,
- cache lookup,
- Arc/reference management,
- page pinning,
- node decode,
- vector clone,
- and poor locality.

This approach should be discarded for RobustPrune.

## 7.3 Recommended Approach 3: vector accessor + bounded scratch preload

```rust
pub trait VectorAccessor {
    type VectorRef<'a>
    where
        Self: 'a;

    fn prefetch(&self, ids: &[NodeId]);

    fn get<'a>(&'a self, id: NodeId) -> Self::VectorRef<'a>;

    fn distance_to_query(&self, query: &[f32], id: NodeId) -> f32;

    fn distance_between(&self, a: NodeId, b: NodeId) -> f32;
}
```

For pruning:

```rust
fn select_neighbors_heuristic(
    query: &[f32],
    candidates: &[Candidate],
    m: usize,
    vectors: &impl VectorAccessor,
) -> Vec<NodeId> {
    let pool_len = (3 * m).min(candidates.len());
    let pool = &candidates[..pool_len];

    vectors.prefetch(&pool.iter().map(|c| c.id).collect::<Vec<_>>());

    let mut selected = Vec::with_capacity(m);

    'candidate: for c in pool {
        for &s in &selected {
            let d_cs = vectors.distance_between(c.id, s);
            if d_cs < c.distance_to_query {
                continue 'candidate;
            }
        }

        selected.push(c.id);
        if selected.len() == m {
            break;
        }
    }

    selected
}
```

This makes the pruning algorithm independent of graph-node storage.

---

## 8. Recommended Architecture for Your Paged Rust HNSW

## 8.1 Separate graph nodes from vectors

Current likely shape:

```text
NodeRecord {
    id,
    level,
    adjacency,
    vector,
    metadata,
}
```

Recommended shape:

```text
GraphNodeRecord {
    id,
    level,
    adjacency_by_layer,
    metadata,
}

VectorRecord {
    id,
    full_vector,
    optional_quantized_code,
}
```

Storage layout:

```text
Graph pages:
    node id -> adjacency lists, layer metadata

Vector pages:
    node id -> full vector bytes or offset into vector slab

Quantized vector slab:
    node id -> PQ/BQ/NVQ/scalar-quantized representation
```

## 8.2 Use a construction scratch arena

For `3M = 48`, use a per-thread scratch structure:

```rust
pub struct PruneScratch {
    ids: SmallVec<[NodeId; 64]>,
    vectors: SmallVec<[AlignedVector; 64]>,
    distances: SmallVec<[f32; 64]>,
}
```

This avoids per-insertion heap churn.

## 8.3 Add batch vector loading

```rust
pub trait VectorStore {
    fn load_vector(&self, id: NodeId, dst: &mut [f32]) -> Result<()>;

    fn load_vectors_batch(
        &self,
        ids: &[NodeId],
        scratch: &mut PruneScratch,
    ) -> Result<()>;

    fn prefetch_vector_pages(&self, ids: &[NodeId]);
}
```

Even if the first implementation simply loops internally, this creates the right abstraction for future optimizations:

- page grouping,
- mmap slice borrowing,
- async prefetch,
- read coalescing,
- compressed-vector scoring,
- and SIMD distance batches.

## 8.4 Keep upper-layer graph data memory resident

Borrow from JVector:

```text
Layer > 0:
    adjacency in memory or aggressively cached

Layer 0:
    paged adjacency on disk
```

Upper layers are small but critical for navigability. Keeping them memory-resident reduces search and insertion I/O.

## 8.5 Use quantized scoring later

After full-vector access is clean, add a compressed scoring tier:

```rust
pub enum DistancePrecision {
    Full,
    ProductQuantized,
    BinaryQuantized,
    ScalarQuantized,
    ApproxThenFull,
}
```

Suggested progression:

1. Full precision vector accessor.
2. Full precision vector cache/slab.
3. SIMD full precision distance.
4. PQ/scalar quantized approximate distance.
5. Approximate pruning plus full precision validation.
6. Disk reranking for large datasets.

---

## 9. What to Borrow from Each System

## 9.1 Borrow from hnswlib

- Keep vector access extremely cheap.
- Avoid object hydration in inner loops.
- Store data in compact, cache-friendly layouts.
- Make distance computation a hot path, not a storage path.

## 9.2 Borrow from Faiss

- Introduce a `DistanceComputer` or `VectorAccessor` abstraction.
- Batch distance computations where possible.
- Specialize distance computation by metric and vector encoding.
- Keep ids and vectors decoupled.

## 9.3 Borrow from DiskANN

- Treat disk as a separate tier, not just slow memory.
- Keep compressed vectors in memory.
- Store full vectors on disk for reranking/validation.
- Design graph layout to minimize random reads.
- Consider Vamana-style pruning if HNSW heuristic remains weak.

## 9.4 Borrow from ScaNN

- Use quantization as a ranking/scoring accelerator.
- Consider anisotropic or score-aware quantization for inner-product workloads.
- Use coarse pruning or partitioning only when graph search alone becomes insufficient.
- Optimize for SIMD from the beginning.

## 9.5 Borrow from JVector

- Combine HNSW hierarchy with DiskANN/Vamana-style layer pruning.
- Keep upper layers in memory.
- Store lower-layer adjacency on disk.
- Use two-pass search: compressed first, accurate second.
- Apply two-pass logic to construction as well as query.
- Design for concurrent construction if this will become production infrastructure.

---

## 10. Concrete Recommendation for Your Current Bug/Optimization Ticket

## 10.1 Do not implement GPU yet

At `15K` vectors and `3M = 48`, GPU is not the right next step. The overhead is in storage access, not raw floating-point throughput.

## 10.2 Do not rely on the node cache

Your node cache may be correct and still inappropriate for this workload.

Graph-node cache responsibilities:

```text
adjacency
levels
metadata
dirty state
serialization
locking
```

Vector-cache responsibilities:

```text
id -> vector bytes/slice
read-mostly access
SIMD alignment
batch prefetch
minimal locking
```

These should be separate.

## 10.3 Implement `VectorAccessor` first

Minimal version:

```rust
pub trait VectorAccessor {
    fn dim(&self) -> usize;

    fn prefetch(&self, ids: &[NodeId]);

    fn copy_vector_into(&self, id: NodeId, dst: &mut [f32]) -> Result<()>;

    fn distance_between(&self, a: NodeId, b: NodeId) -> Result<f32>;
}
```

Better version:

```rust
pub trait VectorAccessor {
    type Ref<'a>
    where
        Self: 'a;

    fn dim(&self) -> usize;
    fn prefetch(&self, ids: &[NodeId]);
    fn get<'a>(&'a self, id: NodeId) -> Result<Self::Ref<'a>>;
    fn distance_between_refs(&self, a: Self::Ref<'_>, b: Self::Ref<'_>) -> f32;
    fn distance_between(&self, a: NodeId, b: NodeId) -> Result<f32>;
}
```

## 10.4 Replace `HashMap` preload with scratch-array preload

Instead of:

```rust
HashMap<NodeId, Vec<f32>>
```

use:

```rust
struct CandidateVector {
    id: NodeId,
    distance_to_query: f32,
    vector: AlignedVec<f32>,
}

SmallVec<[CandidateVector; 64]>
```

For `M=16`, linear scan over 48 elements is fine and often faster than hashing.

## 10.5 Add a three-mode pruning implementation

```rust
pub enum NeighborSelectionStrategy {
    GreedyNearest,
    RobustPruneFull,
    RobustPruneApproxThenFull,
}
```

Use:

- `GreedyNearest` as fallback.
- `RobustPruneFull` for small/medium in-memory or cached builds.
- `RobustPruneApproxThenFull` for larger-than-memory builds.

---

## 11. Suggested Ticket Breakdown

## Ticket 1: Split vector access from graph node loading

**Goal:** Introduce `VectorAccessor` and stop calling `load_node()` from pruning.

Acceptance criteria:

- `select_neighbors` takes a vector accessor.
- No graph-node deserialization occurs during pairwise pruning.
- Existing greedy selection still works.
- RobustPrune can be enabled behind a feature flag or config option.

## Ticket 2: Add bounded prune scratch arena

**Goal:** Avoid repeated allocation and hash lookups during insertion.

Acceptance criteria:

- Per-thread or per-insertion reusable scratch buffer.
- Candidate pool capped at `alpha * M`, default `alpha = 3`.
- No `HashMap` in the hot pruning loop for small candidate pools.

## Ticket 3: Add batch vector preload/prefetch

**Goal:** Load candidate vectors once per insertion.

Acceptance criteria:

- `load_vectors_batch(ids, scratch)` API exists.
- Vector pages are grouped or prefetched when possible.
- Metrics record vector loads per insertion.

## Ticket 4: Add full-vector cache independent from node cache

**Goal:** Reduce vector load overhead while avoiding graph-node cache contention.

Acceptance criteria:

- Cache key: `NodeId`.
- Cache value: compact vector representation only.
- Read path avoids graph locks.
- Metrics distinguish node-cache hits from vector-cache hits.

## Ticket 5: Add SIMD distance backend

**Goal:** Make 34.5M+ distance checks cheap.

Acceptance criteria:

- Scalar fallback.
- SIMD implementation for common dimensions or dynamic dimension loop.
- Criterion benchmark for distance throughput.

## Ticket 6: Add optional quantized scoring tier

**Goal:** Prepare for DiskANN/JVector-style larger-than-memory behavior.

Acceptance criteria:

- Store PQ/scalar/BQ code per vector.
- Approximate distance can be computed without loading full vector.
- Final reranking can use full vectors.

---

## 12. Final Architectural Position

Your current paged HNSW design is trying to combine two worlds:

1. HNSW's memory-resident graph assumptions.
2. A database storage engine's paged, cached, serialized object model.

The friction appears exactly where expected: the HNSW inner loop assumes vector access is cheap, while your storage engine makes vector access a node-loading operation.

The fix is not merely to micro-optimize `load_node()`. The fix is to introduce a distance-oriented access path.

Best target design:

```text
HNSW/JVector-like hierarchy
+ Vamana/DiskANN-inspired diversity pruning
+ separate vector accessor
+ memory-resident compressed scoring tier
+ paged full-vector store
+ full-precision construction/rerank path
```

For the immediate problem, implement:

```text
VectorAccessor + batch preload + prune scratch buffer
```

Before considering:

```text
GPU, advanced PQ, partitioning, or major graph algorithm changes
```

If RobustPrune over 48 candidates is taking hundreds or thousands of seconds at 15K vectors, the algorithm is not the culprit. The abstraction boundary is.

---

## Sources

- hnswlib repository: https://github.com/nmslib/hnswlib
- Faiss HNSW implementation: https://github.com/facebookresearch/faiss/blob/main/faiss/impl/HNSW.cpp
- DiskANN repository: https://github.com/microsoft/DiskANN
- DiskANN paper: https://suhasjs.github.io/files/diskann_neurips19.pdf
- Google ScaNN announcement: https://research.google/blog/announcing-scann-efficient-vector-similarity-search/
- ScaNN README: https://github.com/google-research/google-research/blob/master/scann/README.md
- Anisotropic Vector Quantization paper: https://proceedings.mlr.press/v119/guo20h/guo20h.pdf
- JVector repository: https://github.com/datastax/jvector
- DataStax vector concepts / JVector docs: https://docs.datastax.com/en/dse/6.9/get-started/vector-concepts.html
