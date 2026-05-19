# Nanostore Architecture

**Version**: 1.0  
**Date**: 2026-05-10  
**Status**: Current

---

## Table of Contents

1. [Overview](#overview)
2. [System Architecture](#system-architecture)
3. [Layer Architecture](#layer-architecture)
4. [Component Details](#component-details)
5. [Data Flow](#data-flow)
6. [Concurrency Model](#concurrency-model)
7. [File Format](#file-format)
8. [Performance Characteristics](#performance-characteristics)
9. [Design Decisions](#design-decisions)
10. [Specialty Table Integration](#specialty-table-integration)

---

## Overview

Nanostore is a lightweight, embeddable key-value database designed for single-file storage with ACID transaction support. It provides a layered architecture with pluggable storage engines, MVCC-based concurrency control, and comprehensive durability guarantees through write-ahead logging.

### Key Features

- **Single-file database**: All data stored in one file for easy deployment
- **ACID transactions**: Full transaction support with snapshot isolation
- **Multiple storage engines**: BTree (read-optimized) and LSM (write-optimized)
- **MVCC concurrency**: Non-blocking reads with version chains
- **Write-ahead logging**: Crash recovery and durability
- **Configurable page sizes**: 4KB to 64KB pages
- **Optional compression**: LZ4 and Zstd support
- **Optional encryption**: AES-256-GCM encryption
- **Virtual file system**: Pluggable storage backends (local, memory, cloud)

### Design Philosophy

1. **Simplicity**: Single-file design, minimal dependencies
2. **Correctness**: ACID guarantees, checksums, crash recovery
3. **Performance**: Zero-copy where possible, efficient caching
4. **Flexibility**: Pluggable engines, configurable options
5. **Embeddability**: Library-first design, no external services

---

## System Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                      Application Layer                       │
└─────────────────────────────────────────────────────────────┘
                              │
┌─────────────────────────────────────────────────────────────┐
│                     Transaction Layer                        │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐      │
│  │ Transaction  │  │   Snapshot   │  │   Conflict   │      │
│  │  Manager     │  │   Manager    │  │   Detector   │      │
│  └──────────────┘  └──────────────┘  └──────────────┘      │
└─────────────────────────────────────────────────────────────┘
                              │
┌─────────────────────────────────────────────────────────────┐
│                       Table Layer                            │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐      │
│  │  BTree Table │  │   LSM Table  │  │ Memory Table │      │
│  │  (Ordered)   │  │ (Write-Opt)  │  │   (Fast)     │      │
│  └──────────────┘  └──────────────┘  └──────────────┘      │
└─────────────────────────────────────────────────────────────┘
                              │
┌─────────────────────────────────────────────────────────────┐
│                        Pager Layer                           │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐      │
│  │  Page Cache  │  │  Free List   │  │  Page Table  │      │
│  │   (LRU)      │  │ (Lock-Free)  │  │  (Sharded)   │      │
│  └──────────────┘  └──────────────┘  └──────────────┘      │
│  ┌──────────────┐  ┌──────────────┐                        │
│  │ Compression  │  │  Encryption  │                        │
│  │  (LZ4/Zstd)  │  │ (AES-256-GCM)│                        │
│  └──────────────┘  └──────────────┘                        │
└─────────────────────────────────────────────────────────────┘
                              │
┌─────────────────────────────────────────────────────────────┐
│                      WAL (Write-Ahead Log)                   │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐      │
│  │  WAL Writer  │  │  WAL Reader  │  │   Recovery   │      │
│  │ (Buffered)   │  │  (Iterator)  │  │   Manager    │      │
│  └──────────────┘  └──────────────┘  └──────────────┘      │
└─────────────────────────────────────────────────────────────┘
                              │
┌─────────────────────────────────────────────────────────────┐
│                  VFS (Virtual File System)                   │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐      │
│  │ Local FS     │  │  Memory FS   │  │  Custom FS   │      │
│  │ (Production) │  │  (Testing)   │  │  (Cloud)     │      │
│  └──────────────┘  └──────────────┘  └──────────────┘      │
└─────────────────────────────────────────────────────────────┘
```

---

## Layer Architecture

### 1. Virtual File System (VFS) Layer

**Purpose**: Platform-independent file I/O abstraction

**Components**:
- `FileSystem` trait: Abstract interface for file operations
- `LocalFileSystem`: Native OS file system implementation
- `MemoryFileSystem`: In-memory implementation for testing
- File locking support (exclusive/shared)

**Key Features**:
- Platform abstraction (Windows, Linux, macOS)
- Pluggable backends (local, memory, cloud)
- File locking for concurrent access
- Atomic operations (create, rename, delete)

**File**: `src/vfs/`

---

### 2. Write-Ahead Log (WAL) Layer

**Purpose**: Durability and crash recovery

**Components**:
- `WalWriter`: Buffered write operations
- `WalReader`: Sequential log reading
- `WalRecovery`: Crash recovery logic
- `GroupCommitCoordinator`: Batch commit optimization

**Record Types**:
- `BEGIN`: Start transaction
- `WRITE`: Record write operation (put/delete)
- `COMMIT`: Commit transaction
- `ROLLBACK`: Abort transaction
- `CHECKPOINT`: Mark recovery point

**Key Features**:
- SHA-256 checksums for integrity
- Buffered writes for performance
- Group commit for throughput
- Automatic recovery on startup
- LSN (Log Sequence Number) tracking

**File**: `src/wal/`

---

### 3. Pager Layer

**Purpose**: Block-level storage with page management

**Components**:
- `Pager`: Main page management interface
- `PageCache`: LRU cache with sharding (64 shards)
- `FreeList`: Lock-free page allocation
- `PageTable`: Fine-grained page locking (64 shards)
- `Superblock`: Database metadata
- `FileHeader`: File format identification

**Page Structure**:
```
┌─────────────────────────────────────────┐
│ Page Header (32 bytes)                  │
│  - Page ID (8 bytes)                    │
│  - Page Type (1 byte)                   │
│  - Flags (1 byte)                       │
│  - Data Length (4 bytes)                │
│  - Reserved (18 bytes)                  │
├─────────────────────────────────────────┤
│ Page Data (variable)                    │
│  - Actual content                       │
│  - Compressed if enabled                │
│  - Encrypted if enabled                 │
├─────────────────────────────────────────┤
│ Checksum (32 bytes)                     │
│  - SHA-256 hash                         │
└─────────────────────────────────────────┘
```

**Page Types**:
- `Header`: File header (page 0)
- `Superblock`: Database metadata (page 1)
- `FreeList`: Free page tracking
- `BTreeInternal`: B-Tree internal nodes
- `BTreeLeaf`: B-Tree leaf nodes
- `LSMMemtable`: LSM memtable data
- `LSMSSTable`: LSM sorted string table
- `Data`: Generic data pages

**Key Features**:
- Configurable page sizes (4KB, 8KB, 16KB, 32KB, 64KB)
- Optional compression (LZ4, Zstd)
- Optional encryption (AES-256-GCM)
- SHA-256 checksums
- Lock-free free list (crossbeam SegQueue)
- Sharded cache (64 shards, reduces contention)
- Fine-grained page locking (64 shards)
- Pin table for reference counting

**Concurrency Model**:
- Page-level locking via PageTable (64 shards)
- Lock-free free list operations
- Sharded cache for parallel access
- Pin table prevents use-after-free

**File**: `src/pager/`

---

### 4. Table Layer

**Purpose**: Key-value storage with multiple engine implementations

**Storage Engines**:

#### A. BTree Table (Read-Optimized)

**Structure**:
- B-Tree with configurable order (default: 64)
- Internal nodes: Keys + child pointers
- Leaf nodes: Key-value pairs + version chains
- Linked leaf nodes for range scans

**Features**:
- Ordered key storage
- Efficient range queries
- Predictable performance
- MVCC via version chains
- Node split/merge operations

**Use Cases**:
- Read-heavy workloads
- Range queries
- Ordered iteration
- Predictable latency

**File**: `src/table/btree/`

#### B. LSM Table (Write-Optimized)

**Structure**:
```
Writes → Memtable → Immutable Memtable → L0 SSTable
                                             ↓
                                        Compaction
                                             ↓
                                   L1, L2, ..., Ln SSTables
```

**Components**:
- `Memtable`: In-memory write buffer (skip list)
- `SSTable`: Immutable sorted string tables
- `BloomFilter`: Probabilistic filters per SSTable
- `Manifest`: Version and file metadata
- `CompactionManager`: Background compaction

**Features**:
- Write-optimized (sequential writes)
- Bloom filters reduce disk I/O
- Leveled compaction strategy
- Per-block compression
- MVCC via version chains

**Use Cases**:
- Write-heavy workloads
- Append-only patterns
- Time-series data
- Log storage

**File**: `src/table/lsm/`

#### C. Memory Table (Fast)

**Structure**:
- In-memory B-Tree or skip list
- No persistence
- No compression/encryption overhead

**Use Cases**:
- Temporary data
- Caching
- Testing
- Session storage

**File**: `src/table/btree/memory.rs`

**Common Traits**:
- `PointLookup`: Get operations with snapshot LSN
- `OrderedScan`: Range queries with snapshot LSN
- `MutableTable`: Put/delete operations
- `Flushable`: Persist to disk
- `MemoryAware`: Memory pressure handling
- `Maintainable`: Compaction, vacuum, repair

**File**: `src/table/`

---

### 5. Transaction Layer

**Purpose**: ACID transaction support with MVCC

**Components**:
- `Transaction`: Transaction context and operations
- `TransactionId`: Unique transaction identifier
- `VersionChain`: Multi-version storage
- `ConflictDetector`: Write-write conflict detection
- `DeadlockDetector`: Wait-for graph cycle detection
- `Snapshot`: Point-in-time view

**Transaction Lifecycle**:
```
BEGIN → READ/WRITE Operations → COMMIT/ROLLBACK
  ↓           ↓                      ↓
 WAL      Version Chain          WAL + Apply
```

**MVCC Implementation**:
- Each write creates a new version
- Versions linked in chains
- Snapshot LSN determines visibility
- Garbage collection removes old versions

**Isolation Level**: Snapshot Isolation
- Reads see consistent snapshot
- Writes detect conflicts
- No phantom reads
- Serializable for read-only transactions

**Key Features**:
- ACID guarantees
- Non-blocking reads
- Write-write conflict detection
- Snapshot isolation
- Version chain management
- Automatic garbage collection

**File**: `src/txn/`

---

## Component Details

### Pager Concurrency Architecture

The pager implements a sophisticated multi-level concurrency model:

**1. PageTable (Fine-Grained Locking)**
- 64 shards using page_id % 64
- Each shard has independent RwLock
- Enables concurrent access to different pages
- Reduces lock contention by ~64x

**2. Lock-Free FreeList**
- Uses crossbeam SegQueue (lock-free)
- Atomic counters for statistics
- No contention on allocation/deallocation
- Scales linearly with threads

**3. Sharded Cache**
- 64 independent cache shards
- Each shard has own RwLock and LRU
- Parallel cache operations
- Reduces cache lock contention

**4. Pin Table**
- Reference counting for pages
- Prevents use-after-free
- Atomic operations
- Works with page locks

**Lock Ordering** (prevents deadlocks):
1. Page-level lock (PageTable)
2. File lock (VFS)
3. Superblock/Header locks

**Performance**:
- Target: 3-5x throughput @ 8 threads
- Baseline: ~130K ops/sec @ 8 threads
- Goal: ~500K ops/sec @ 8 threads

### LSM Compaction Strategy

**Leveled Compaction**:
- L0: Overlapping SSTables (from memtable flushes)
- L1-Ln: Non-overlapping SSTables
- Exponential level sizes (10x growth)
- Background compaction thread

**Compaction Triggers**:
- L0 file count threshold (default: 4)
- Level size threshold
- Manual trigger
- Scheduled compaction

**Compaction Process**:
1. Select files to compact
2. Merge sort with version filtering
3. Write new SSTables
4. Update manifest
5. Delete old files

**Bloom Filters**:
- One per SSTable
- Configurable false positive rate (default: 1%)
- Reduces unnecessary disk reads
- Stored in SSTable footer

### Version Chain Management

**Structure**:
```rust
struct VersionChain {
    versions: Vec<Version>,
}

struct Version {
    lsn: LogSequenceNumber,
    txn_id: TransactionId,
    value: Option<Vec<u8>>,  // None = deleted
    next: Option<Box<Version>>,
}
```

**Visibility Rules**:
- Version visible if: version.lsn <= snapshot.lsn
- Traverse chain to find visible version
- Stop at first visible version

**Garbage Collection**:
- Remove versions older than oldest active snapshot
- Triggered during compaction (LSM)
- Separate GC thread (BTree)
- Configurable retention period

---

## Data Flow

### Write Path

```
Application
    ↓
Transaction.put(key, value)
    ↓
WAL.write_operation(txn_id, table_id, PUT, key, value)
    ↓
Table.put(key, value, txn_id, lsn)
    ↓
[BTree Path]                    [LSM Path]
    ↓                               ↓
BTree.insert(key, version)      Memtable.insert(key, version)
    ↓                               ↓
Pager.write_page(page_id)       (in memory, flush later)
    ↓                               ↓
VFS.write_at_offset()           Memtable full?
    ↓                               ↓
Disk                            Flush to SSTable
                                    ↓
                                Pager.write_page()
                                    ↓
                                VFS.write_at_offset()
                                    ↓
                                Disk
```

### Read Path

```
Application
    ↓
Transaction.get(key)
    ↓
snapshot_lsn = transaction.snapshot_lsn
    ↓
Table.get(key, snapshot_lsn)
    ↓
[BTree Path]                    [LSM Path]
    ↓                               ↓
BTree.search(key)               Check Memtable
    ↓                               ↓
Pager.read_page(page_id)        Found? Return
    ↓                               ↓
Cache hit? Return               Check Immutable Memtable
    ↓                               ↓
VFS.read_at_offset()            Found? Return
    ↓                               ↓
Decompress/Decrypt              For each level (L0, L1, ...):
    ↓                               ↓
Verify checksum                 Check Bloom filter
    ↓                               ↓
Cache page                      Bloom says maybe?
    ↓                               ↓
Return value                    Pager.read_page(sstable)
                                    ↓
                                Binary search in SSTable
                                    ↓
                                Found? Return
                                    ↓
                                Not found
```

### Transaction Commit Path

```
Application
    ↓
Transaction.commit()
    ↓
ConflictDetector.check_conflicts()
    ↓
Conflicts? → ROLLBACK
    ↓
No conflicts
    ↓
WAL.write_commit(txn_id)
    ↓
WAL.flush()  (fsync)
    ↓
Apply writes to tables
    ↓
Update version chains
    ↓
Release locks
    ↓
Return success
```

### Recovery Path

```
Database.open()
    ↓
Check for WAL file
    ↓
WAL exists?
    ↓
WalRecovery.recover()
    ↓
Read all WAL records
    ↓
Build transaction state:
  - Committed transactions
  - Active transactions
  - Rolled back transactions
    ↓
Apply committed writes
    ↓
Rollback active transactions
    ↓
Truncate WAL
    ↓
Database ready
```

---

## Concurrency Model

### Read-Write Concurrency

**Readers**:
- Non-blocking (MVCC)
- Read from snapshot
- No locks on data
- Only cache/page locks

**Writers**:
- Write to WAL first
- Create new versions
- Detect conflicts
- Commit atomically

**Isolation**: Snapshot Isolation
- Readers see consistent snapshot
- Writers create new versions
- No read-write conflicts
- Write-write conflicts detected

### Lock Hierarchy

```
1. Transaction locks (highest)
   - Held for transaction duration
   - Prevent concurrent modifications

2. Page locks (PageTable)
   - Held during page I/O
   - Fine-grained (64 shards)

3. Cache locks (sharded)
   - Held during cache operations
   - 64 independent shards

4. File lock (VFS)
   - Held during I/O only
   - Minimal duration

5. Superblock/Header locks (lowest)
   - Held during metadata updates
   - Infrequent access
```

### Deadlock Prevention

**Strategies**:
1. **Lock ordering**: Always acquire in same order
2. **Timeout**: Abort if lock not acquired
3. **No nested locks**: Release before acquiring next
4. **Lock-free structures**: FreeList uses lock-free queue
5. **Deadlock detection**: Wait-for graph cycle detection for transactions

**Transaction Deadlock Detection**:
- `DeadlockDetector` maintains a wait-for graph
- Tracks which transactions are waiting for locks held by other transactions
- Uses depth-first search (DFS) to detect cycles in the wait-for graph
- When a cycle is detected, one transaction is aborted to break the deadlock
- Prevents transactions from waiting indefinitely for locks

**Wait-For Graph**:
```
Transaction A → Transaction B  (A waits for B)
Transaction B → Transaction C  (B waits for C)
Transaction C → Transaction A  (C waits for A) ← Cycle detected!
```

---

## File Format

### Database File Structure

```
┌─────────────────────────────────────────┐
│ Page 0: File Header                     │
│  - Magic number: "Nanostore\0\0"          │
│  - Version: 1                           │
│  - Page size: 4096/8192/16384/...      │
│  - Compression: None/LZ4/Zstd          │
│  - Encryption: None/AES-256-GCM        │
│  - Checksum: SHA-256                    │
├─────────────────────────────────────────┤
│ Page 1: Superblock                      │
│  - Total pages                          │
│  - Free pages                           │
│  - First free list page                 │
│  - Next page ID                         │
│  - Transaction counter                  │
│  - Last checkpoint LSN                  │
│  - Root B-Tree page                     │
├─────────────────────────────────────────┤
│ Page 2+: Data Pages                     │
│  - Free list pages                      │
│  - B-Tree nodes                         │
│  - LSM SSTables                         │
│  - User data                            │
└─────────────────────────────────────────┘
```

### WAL File Structure

```
┌─────────────────────────────────────────┐
│ WAL Header                              │
│  - Magic number                         │
│  - Version                              │
│  - Start LSN                            │
├─────────────────────────────────────────┤
│ Record 1                                │
│  - LSN                                  │
│  - Record type (BEGIN/WRITE/COMMIT)     │
│  - Transaction ID                       │
│  - Data length                          │
│  - Data                                 │
│  - Checksum (SHA-256)                   │
├─────────────────────────────────────────┤
│ Record 2                                │
│  ...                                    │
├─────────────────────────────────────────┤
│ Record N                                │
│  ...                                    │
└─────────────────────────────────────────┘
```

### SSTable Format (LSM)

```
┌─────────────────────────────────────────┐
│ Data Block 1                            │
│  - Key-value pairs (sorted)             │
│  - Compressed (optional)                │
│  - Encrypted (optional)                 │
├─────────────────────────────────────────┤
│ Data Block 2                            │
│  ...                                    │
├─────────────────────────────────────────┤
│ Index Block                             │
│  - Block offsets                        │
│  - First key per block                  │
├─────────────────────────────────────────┤
│ Bloom Filter                            │
│  - Bit array                            │
│  - Hash function count                  │
├─────────────────────────────────────────┤
│ Footer                                  │
│  - Index block offset                   │
│  - Bloom filter offset                  │
│  - Min/max key                          │
│  - Entry count                          │
│  - Checksum                             │
└─────────────────────────────────────────┘
```

---

## Performance Characteristics

### BTree Table

**Time Complexity**:
- Get: O(log n)
- Put: O(log n)
- Delete: O(log n)
- Range scan: O(log n + k) where k = result count

**Space Complexity**:
- O(n) for n entries
- ~50-75% page utilization

**Best For**:
- Read-heavy workloads
- Range queries
- Ordered iteration
- Predictable latency

### LSM Table

**Time Complexity**:
- Get: O(log n) with bloom filter optimization
- Put: O(1) amortized (memtable insert)
- Delete: O(1) amortized (tombstone)
- Range scan: O(log n + k) with merge

**Space Complexity**:
- O(n) for n entries
- Higher write amplification (compaction)
- Lower space amplification (compression)

**Best For**:
- Write-heavy workloads
- Append-only patterns
- Time-series data
- High throughput

### Cache Performance

**Sharded Cache** (64 shards):
- Hit rate: ~90% for hot data
- Contention: Reduced by 64x
- Scalability: Linear to 64 threads

**LRU Eviction**:
- O(1) access
- O(1) eviction
- Predictable behavior

---

## Design Decisions

See [ADR Index](./adrs/README.md) for detailed Architecture Decision Records.

### Key Decisions

1. **Single-file design**: Simplicity, easy deployment
2. **Page-based storage**: Standard approach, efficient caching
3. **MVCC concurrency**: Non-blocking reads, snapshot isolation
4. **WAL for durability**: Crash recovery, ACID guarantees
5. **Multiple storage engines**: Flexibility for different workloads
6. **Sharded concurrency**: Fine-grained locking, scalability
7. **Lock-free free list**: Eliminates allocation bottleneck
8. **Bloom filters**: Reduces disk I/O for LSM
9. **Optional compression/encryption**: Security and space efficiency
10. **VFS abstraction**: Platform independence, testability

---

## Specialty Table Integration

Nanostore supports specialty table engines that provide domain-specific query capabilities beyond standard key-value operations. These are integrated into the transaction layer using a trait-based design.

### Specialty Table Types

| Type | Trait | Engine Kinds | Use Case |
|------|-------|--------------|----------|
| Approximate Membership | `ApproximateMembership` | Bloom | Probabilistic key existence checks |
| Full-Text Search | `FullTextSearch` | FullText | Document indexing and search |
| Vector Search | `VectorSearch` | Hnsw | Similarity search for embeddings |
| GeoSpatial | `GeoSpatial` | RTree | Location-based queries |
| Time Series | `TimeSeries` | TimeSeries | Temporal data management |
| Graph | `GraphAdjacency` | Graph | Edge traversal and graph queries |

### Transaction Integration Pattern

The `Transaction<FS>` struct implements each specialty table trait directly, enabling uniform transaction semantics:

```rust
impl<FS: FileSystem> ApproximateMembership for Transaction<FS> { ... }
impl<FS: FileSystem> FullTextSearch for Transaction<FS> { ... }
impl<FS: FileSystem> VectorSearch for Transaction<FS> { ... }
impl<FS: FileSystem> GeoSpatial for Transaction<FS> { ... }
impl<FS: FileSystem> TimeSeries for Transaction<FS> { ... }
impl<FS: FileSystem> GraphAdjacency for Transaction<FS> { ... }
```

Each implementation:
1. Validates transaction state (must be `Active`)
2. Checks table context is set via `current_table_id`
3. Logs operation to WAL with type-specific `WriteOpType`
4. Records operation in type-specific write set
5. Delegates to underlying engine for read/query operations

### Write Set Architecture

Each specialty table type has a dedicated write set field in `Transaction`:

```rust
pub struct Transaction<FS: FileSystem> {
    // ... core fields ...
    bloom_write_set: HashSet<(TableId, Vec<u8>)>,
    graph_write_set: Vec<(TableId, GraphEdgeOp)>,
    timeseries_write_set: RefCell<Vec<(TableId, TimeSeriesOp)>>,
    vector_write_set: RwLock<Vec<(TableId, VectorOp)>>,
    geospatial_write_set: Vec<(TableId, GeoSpatialOp)>,
    fulltext_write_set: Vec<(TableId, FullTextOp)>,
}
```

The synchronization primitives (`RefCell`, `RwLock`) enable interior mutability for trait methods that take `&self` instead of `&mut self`.

### Scoped Helper Methods

For dyn-compatible traits, scoped helpers manage table context automatically:

```rust
pub fn with_bloom<F, R>(&mut self, table_id: TableId, f: F) -> TransactionResult<R>
where
    F: FnOnce(&mut dyn ApproximateMembership) -> TableResult<R>;
```

Traits using GATs (Generic Associated Types) like `GraphAdjacency` and `TimeSeries` require manual context management via `with_table()` and `clear_table_context()`.

### Commit and Rollback

On `commit()`, specialty write sets are applied after the main write set. On `rollback()`, write sets are discarded without application.

**Note**: Some specialty tables (PagedFullTextIndex, PagedRTree) require interior mutability updates before full commit-time application is enabled. Operations are logged to WAL for durability, but actual application may be deferred.

For complete documentation, see [Specialty Table Transactions](./SPECIALTY_TABLE_TRANSACTIONS.md).

---

## Related Documents

- [File Format Specification](./FILE_FORMAT.md)
- [Concurrency Model](./PAGER_CONCURRENCY_COMPLETE.md)
- [Specialty Table Transactions](./SPECIALTY_TABLE_TRANSACTIONS.md)
- [LSM Implementation](./archive/WAL_IMPLEMENTATION.md)
- [Architecture Decision Records](./adrs/)
- [Phase 1 Completion](./PHASE1_COMPLETION_SUMMARY.md)

---

**Last Updated**: 2026-05-10  
**Authors**: Hans W. Uhlig, Bob (AI Assistant)