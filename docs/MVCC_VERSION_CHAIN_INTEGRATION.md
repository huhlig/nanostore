# MVCC Version Chain Integration Plan

## Overview

This document tracks the integration of MVCC (Multi-Version Concurrency Control) version chains across all storage engines in Nanostore. The `VersionChain` structure exists in `src/txn/version.rs` and provides snapshot isolation through version visibility checking.

## Current Status

### ✅ Engines WITH VersionChain Integration

1. **MemoryBTree** (`src/table/btree/memory.rs`)
   - Status: ✅ Fully integrated
   - Stores `BTreeMap<Vec<u8>, VersionChain>`
   - Implements visibility checking in cursors
   - Has `commit_versions()` method

2. **PagedBTree** (`src/table/btree/paged.rs`)
   - Status: ✅ Integrated
   - Stores version chains in leaf nodes
   - Serializes/deserializes chains with postcard
   - Has `commit_chain_recursive()` helper

3. **MemoryHashTable** (`src/table/hash/memory.rs`)
   - Status: ✅ Fully integrated
   - Stores `HashMap<Vec<u8>, VersionChain>`
   - Implements visibility checking
   - Has `commit_versions()` method

4. **MemoryART** (`src/table/art/memory.rs`)
   - Status: ✅ Integrated
   - Stores chains in leaf nodes
   - Uses `prepend()` for new versions

5. **LsmTree** (`src/table/lsm/mod.rs`)
   - Status: ✅ Fully integrated
   - Memtable stores `BTreeMap<Vec<u8>, VersionChain>`
   - SSTables store version chains in data blocks
   - Iterator merges chains from multiple sources
   - Compaction preserves version chains

### ✅ Engines WITH VersionChain Integration (Continued)

6. **TimeSeriesTable** (`src/table/timeseries/mod.rs`)
   - Status: ✅ Integrated
   - Stores `BTreeMap<i64, VersionChain>` in TimeBucket
   - Implements transaction-aware methods: `append_point_tx`, `scan_series_snapshot`, `latest_before_snapshot`
   - Has `commit_versions()` and `vacuum()` methods
   - Serializes/deserializes chains with postcard

### ⏳ Engines WITHOUT VersionChain Integration (Nanostore-302)

**Status**: Blocked by Nanostore-8xz (PagedRTree interior mutability) and Nanostore-ckm (transaction commit path)

See `docs/SPECIALTY_TABLE_MVCC_INTEGRATION_PLAN.md` for detailed implementation plan.

1. **PagedRTree** (`src/table/rtree/paged.rs`)
   - Current: Stores geometry in R-tree nodes
   - Need: Version chains for geometry updates
   - Priority: MEDIUM (geospatial queries)
   - Blocker: Nanostore-8xz (interior mutability issue)

2. **PagedHnswVector** (`src/table/hnsw/paged.rs`)
   - Current: HNSW graph for vector search
   - Need: Version chains for vector updates
   - Priority: MEDIUM (vector search)

3. **PagedFullTextIndex** (`src/table/fulltext/mod.rs`)
   - Current: Inverted index with posting lists
   - Need: Version chains for document updates
   - Priority: MEDIUM (full-text search)

4. **Blob Tables** (`src/table/blob/*.rs`)
   - Current: Store large binary objects
   - Need: Version chains for blob metadata
   - Priority: MEDIUM (large object storage)

5. **PagedBloomFilter** (`src/table/bloom/paged.rs`)
   - Current: Append-only bit array
   - Need: Possibly N/A (bloom filters are probabilistic)
   - Priority: LOW (may not need MVCC)

6. **AppendLog** (`src/table/appendlog/mod.rs`)
   - Current: Append-only log
   - Need: Possibly N/A (append-only by design)
   - Priority: LOW (may not need MVCC)

## VersionChain API

### Core Structure

```rust
pub struct VersionChain {
    pub value: Vec<u8>,
    pub created_by: TransactionId,
    pub commit_lsn: Option<LogSequenceNumber>,
    pub prev_version: Option<Box<VersionChain>>,
}
```

### Key Methods

1. **`new(value, created_by)`** - Create new version
2. **`commit(lsn)`** - Mark version as committed
3. **`prepend(value, created_by)`** - Add new version to chain
4. **`find_visible_version(&snapshot)`** - Find visible version for snapshot
5. **`vacuum(min_visible_lsn)`** - Remove old versions

## Integration Patterns

### Pattern 1: In-Memory Tables (BTree, Hash, ART)

```rust
// Storage
data: Arc<RwLock<BTreeMap<Vec<u8>, VersionChain>>>

// Write
let prev_version = data.get(key).map(|chain| Box::new(chain.clone()));
let new_chain = VersionChain {
    value: value.to_vec(),
    created_by: tx_id,
    commit_lsn: None, // Uncommitted
    prev_version,
};
data.insert(key.to_vec(), new_chain);

// Read with visibility
let snapshot = Snapshot::new(...);
if let Some(chain) = data.get(key) {
    if let Some(value) = chain.find_visible_version(&snapshot) {
        return Some(value);
    }
}

// Commit
for chain in data.values_mut() {
    if chain.created_by == tx_id && chain.commit_lsn.is_none() {
        chain.commit(commit_lsn);
    }
}
```

### Pattern 2: Paged Tables (PagedBTree, LSM)

```rust
// Storage in leaf nodes
struct LeafEntry {
    key: Vec<u8>,
    chain: VersionChain,
}

// Serialize/deserialize with postcard
let chain_bytes = postcard::to_allocvec(&chain)?;
let chain: VersionChain = postcard::from_bytes(&bytes)?;

// Commit recursively
fn commit_chain_recursive(
    chain: &mut VersionChain,
    tx_id: TransactionId,
    commit_lsn: LogSequenceNumber,
) {
    if chain.created_by == tx_id && chain.commit_lsn.is_none() {
        chain.commit(commit_lsn);
    }
    if let Some(prev) = &mut chain.prev_version {
        commit_chain_recursive(prev, tx_id, commit_lsn);
    }
}
```

### Pattern 3: Specialty Tables (Graph, TimeSeries)

For specialty tables, version chains track operations rather than just values:

```rust
// Graph edges
struct EdgeVersion {
    edge_id: u64,
    source: Vec<u8>,
    target: Vec<u8>,
    label: Vec<u8>,
    weight: Option<f64>,
    deleted: bool, // Tombstone
}

// Store as VersionChain
let edge_bytes = postcard::to_allocvec(&edge_version)?;
let chain = VersionChain::new(edge_bytes, tx_id);

// TimeSeries points
struct TimePoint {
    timestamp: i64,
    value: Vec<u8>,
    deleted: bool, // Tombstone
}
```

## Implementation Phases

### Phase 1: High Priority Specialty Tables ✅ CURRENT

1. **TimeSeriesTable** - Add version chains to bucket storage
2. **MemoryGraphTable** - Add version chains to edge storage
3. Update transaction layer to commit versions in specialty tables

### Phase 2: Geospatial and Vector Tables

1. **PagedRTree** - Add version chains to geometry storage
2. **PagedHnswVector** - Add version chains to vector storage

### Phase 3: Full-Text and Blob Tables

1. **PagedFullTextIndex** - Add version chains to posting lists
2. **Blob Tables** - Add version chains to blob metadata

### Phase 4: Optimization and Cleanup

1. Implement vacuum/garbage collection across all tables
2. Add metrics for version chain length
3. Optimize memory usage for long chains
4. Add tests for snapshot isolation

## Transaction Layer Integration

### Current State

The `Transaction` struct in `src/txn/transaction.rs` has:
- Write tracking for specialty tables (graph, timeseries, vector, etc.)
- Commit logic that writes to WAL
- BUT: Missing explicit version chain commit calls

### Required Changes

1. **Add commit_versions() calls in Transaction::commit()**

```rust
// After WAL commit, before returning
if let Some(table) = self.get_table(table_id) {
    if let Some(writer) = table.writer(self.txn_id, self.snapshot_lsn) {
        writer.commit_versions(commit_lsn)?;
    }
}
```

2. **Add vacuum support**

```rust
pub fn vacuum_old_versions(&self, min_visible_lsn: LogSequenceNumber) -> Result<u64> {
    let mut removed = 0;
    for table in self.tables.values() {
        removed += table.vacuum(min_visible_lsn)?;
    }
    Ok(removed)
}
```

## Testing Strategy

### Unit Tests

1. **Version visibility** - Test snapshot isolation
2. **Commit/rollback** - Test version chain updates
3. **Vacuum** - Test garbage collection
4. **Concurrent reads** - Test MVCC with multiple snapshots

### Integration Tests

1. **Cross-table transactions** - Test version chains across tables
2. **Long-running transactions** - Test with many versions
3. **Recovery** - Test version chain persistence

### Example Test

```rust
#[test]
fn test_mvcc_snapshot_isolation() {
    let table = MemoryBTree::new(TableId::from(1), "test".to_string());
    
    // Transaction 1: Write key1=v1
    let mut writer1 = table.writer(TransactionId::from(1), LogSequenceNumber::from(1)).unwrap();
    writer1.put(b"key1", b"v1").unwrap();
    writer1.flush().unwrap();
    writer1.commit_versions(LogSequenceNumber::from(10)).unwrap();
    
    // Transaction 2: Read at LSN 10 (should see v1)
    let reader2 = table.reader(LogSequenceNumber::from(10)).unwrap();
    assert_eq!(reader2.get(b"key1", LogSequenceNumber::from(10)).unwrap(), Some(ValueBuf(b"v1".to_vec())));
    
    // Transaction 3: Write key1=v2
    let mut writer3 = table.writer(TransactionId::from(3), LogSequenceNumber::from(10)).unwrap();
    writer3.put(b"key1", b"v2").unwrap();
    writer3.flush().unwrap();
    writer3.commit_versions(LogSequenceNumber::from(20)).unwrap();
    
    // Transaction 2 still sees v1 (snapshot isolation)
    assert_eq!(reader2.get(b"key1", LogSequenceNumber::from(10)).unwrap(), Some(ValueBuf(b"v1".to_vec())));
    
    // New reader at LSN 20 sees v2
    let reader4 = table.reader(LogSequenceNumber::from(20)).unwrap();
    assert_eq!(reader4.get(b"key1", LogSequenceNumber::from(20)).unwrap(), Some(ValueBuf(b"v2".to_vec())));
}
```

## Performance Considerations

### Memory Usage

- Version chains grow with write frequency
- Need periodic vacuum to reclaim memory
- Consider max chain length limits

### Read Performance

- Visibility checking is O(chain_length)
- Most reads hit first version (common case)
- Snapshot watermark optimization helps

### Write Performance

- Creating new versions is O(1)
- Prepending to chain is efficient
- Commit is O(versions_in_transaction)

## Next Steps

1. ✅ Document current state (this document)
2. ✅ Integrate TimeSeriesTable with VersionChain
3. ⏳ Integrate MemoryGraphTable with VersionChain
4. ⏳ Add commit_versions() calls in Transaction::commit()
5. ⏳ Add vacuum support
6. ✅ Add comprehensive MVCC tests for TimeSeriesTable
7. ⏳ Update existing persistence tests for new MVCC API
8. ⏳ Integrate remaining specialty tables

## References

- `src/txn/version.rs` - VersionChain implementation
- `src/snap.rs` - Snapshot visibility logic
- `src/table/btree/memory.rs` - Reference implementation
- `src/table/lsm/mod.rs` - Complex multi-source implementation
- `docs/TRANSACTION_SUPPORT.md` - Transaction layer design

---

*Last Updated: 2026-05-15*
*Status: Phase 1 - Planning Complete*