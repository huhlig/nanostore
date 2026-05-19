# Problem Statement: Table and Index Implementation

**Date**: 2026-05-08  
**Context**: Nanostore Database Development

## Original Task

Review [`embedded_kv_traits.rs`](../src/embedded_kv_traits.rs) and design comprehensive table and index interfaces for the Nanostore embedded database system.

### Specific Requirements

1. **Evaluate existing trait definitions** in [`embedded_kv_traits.rs`](../src/embedded_kv_traits.rs:1-1841)
2. **Design table implementations** (BTree and LSM)
3. **Design index structures** (ordered, hash, specialized)
4. **Define transaction coordination** with MVCC support
5. **Create catalog system** for metadata management
6. **Provide implementation roadmap** with realistic timeline

## Why This Matters

### 1. Foundation for Higher-Level Abstractions

Nanostore aims to support multiple database paradigms on a single storage kernel:

- **Relational databases** - SQL tables with ACID transactions
- **Document stores** - JSON/BSON collections with flexible schemas
- **Graph databases** - Vertex/edge storage with traversal indexes
- **Time-series databases** - Efficient temporal data storage
- **Vector databases** - Similarity search with HNSW/IVF indexes

**Without proper table/index infrastructure, none of these higher-level features are possible.**

### 2. Performance and Scalability

The table and index layer directly impacts:

- **Query performance** - Index selection and scan efficiency
- **Write throughput** - Batch operations and compaction strategies
- **Memory efficiency** - Cache utilization and eviction policies
- **Concurrency** - Multi-version concurrency control (MVCC)
- **Storage efficiency** - Compression and page utilization

### 3. Correctness and Reliability

Critical correctness properties depend on this layer:

- **ACID guarantees** - Atomicity, consistency, isolation, durability
- **Crash recovery** - WAL replay and catalog reconstruction
- **Consistency verification** - Index integrity and constraint checking
- **Data integrity** - Checksums and corruption detection

### 4. Developer Experience

A well-designed trait system enables:

- **Clear abstractions** - Easy to understand and use
- **Composability** - Mix and match capabilities
- **Testability** - Mock implementations for testing
- **Extensibility** - Add new table/index types without breaking changes

## Current State

### What Exists

✅ **Solid infrastructure layer**:
- [`Pager`](../../src/pager.rs) - Page-based storage with caching
- [`WAL`](../../src/wal.rs) - Write-ahead logging for durability
- [`VFS`](../../src/vfs.rs) - Virtual file system abstraction
- [`embedded_kv_traits.rs`](../src/embedded_kv_traits.rs) - Comprehensive trait definitions (1,841 lines)

### What's Missing

❌ **Implementation layer**:
- [`table/btree.rs`](../../src/table/btree.rs) - Empty file
- [`table/lsm.rs`](../../src/table/lsm.rs) - Empty file
- Transaction coordinator - Not implemented
- Catalog system - Not implemented
- Index implementations - Not implemented

### The Gap

We have excellent **interfaces** (traits) and **infrastructure** (pager, WAL, VFS), but no **implementations** connecting them. This is like having a well-designed API and a solid database engine, but no actual tables or indexes.

## Key Challenges

### 1. Complexity Management

- **MVCC** adds significant complexity to all operations
- **Concurrency** requires careful lock-free or fine-grained locking
- **Crash recovery** must handle partial writes and torn pages
- **Compaction** in LSM trees is algorithmically complex

### 2. Performance Trade-offs

- **BTree vs LSM** - Read-optimized vs write-optimized
- **Memory vs disk** - Cache size vs I/O overhead
- **Consistency vs performance** - Synchronous vs asynchronous operations
- **Space vs time** - Compression overhead vs storage savings

### 3. Correctness Requirements

- **Snapshot isolation** - Consistent reads without blocking writes
- **Serializability** - Prevent anomalies in concurrent transactions
- **Durability** - Survive crashes without data loss
- **Integrity** - Maintain index consistency with table data

### 4. Design Decisions

Many architectural choices require careful consideration:

- BTree node order and split strategies
- LSM compaction policies (size-tiered vs leveled)
- MVCC garbage collection timing
- Memory budget allocation
- Index maintenance strategies (sync vs async)

## Success Criteria

A successful implementation will:

1. ✅ **Leverage existing traits** - Don't reinvent, use what's already designed
2. ✅ **Provide concrete designs** - Detailed enough to implement
3. ✅ **Address all gaps** - Transaction coordinator, catalog, indexes
4. ✅ **Include testing strategy** - Unit, integration, and performance tests
5. ✅ **Realistic timeline** - Phased approach with milestones
6. ✅ **Document trade-offs** - Clear rationale for design decisions
7. ✅ **Identify risks** - Known challenges and mitigation strategies

## Scope

### In Scope

- BTreeTable implementation design
- LSMTable implementation design
- Transaction coordinator with MVCC
- Catalog system for metadata
- 8 index types (ordered, hash, bloom, full-text, vector, graph, spatial, time-series)
- Testing strategy
- Implementation roadmap

### Out of Scope

- Actual code implementation (design only)
- Query optimizer
- SQL parser
- Network protocol
- Replication
- Distributed consensus

## Related Documents

- **[Key Findings](KEY_FINDINGS.md)** - What we discovered during analysis
- **[Design Decisions](DESIGN_DECISIONS.md)** - Architectural choices made
- **[TABLE_INDEX_IMPLEMENTATION_DESIGN.md](TABLE_INDEX_IMPLEMENTATION_DESIGN.md)** - Detailed designs

---

**Next**: See [Key Findings](KEY_FINDINGS.md) for analysis results.