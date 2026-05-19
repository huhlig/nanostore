# Specialty Table MVCC Integration Plan

## Overview

This document outlines the plan for integrating MVCC (Multi-Version Concurrency Control) support into the remaining specialty tables: PagedRTree, PagedHnswVector, PagedFullTextIndex, and Blob tables.

## Current Status

### ✅ Completed MVCC Integrations
- **TimeSeriesTable**: Stores `BTreeMap<i64, VersionChain>` in TimeBucket, implements `commit_versions()` and `vacuum()`
- **MemoryGraphTable**: Stores `HashMap<(source, label, edge_id), VersionChain>` for edges, implements `commit_versions()`

### ❌ Pending MVCC Integrations (Nanostore-302)
1. **PagedRTree** (geospatial indexing)
2. **PagedHnswVector** (vector search)
3. **PagedFullTextIndex** (full-text search)
4. **Blob tables** (PagedBlob, MemoryBlob, FileBlob)

## Blocking Issues

Before implementing MVCC for these tables, the following issues must be resolved:

### 1. PagedRTree Transaction Commit (Nanostore-8xz, Priority 1)
**Problem**: PagedRTree methods require `&mut self` but the engine is stored behind `Arc`. GeoSpatial operations are logged to WAL but NOT applied to storage during commit.

**Location**: `src/txn/transaction.rs:1723`

**Impact**: Cannot implement proper MVCC without fixing the interior mutability issue first.

**Solution Options**:
- Use `RwLock` or `RefCell` for interior mutability
- Redesign PagedRTree to support concurrent access
- Implement a write buffer pattern similar to LSM Tree

### 2. Transaction Commit Path (Nanostore-ckm, Priority 2)
**Problem**: Transaction commit doesn't call `commit_versions()` for all engines that use VersionChain.

**Impact**: Even after adding VersionChain support, changes won't be committed properly.

**Required**: Update `Transaction::commit()` to call `commit_versions()` for all specialty tables.

## Architecture Considerations

### Challenge: Index vs Data Storage

Specialty tables are **index structures** that reference data stored elsewhere:
- **R-Tree**: Indexes geometries by spatial location
- **HNSW**: Indexes vectors by similarity
- **FullText**: Indexes documents by terms
- **Blob**: Stores large binary objects

### MVCC Integration Approaches

#### Approach 1: Version Chains in Leaf Nodes (Recommended for R-Tree, HNSW)
Store VersionChain for each indexed entry:

```rust
// Current LeafEntry
pub struct LeafEntry {
    pub mbr: Mbr,
    pub object_id: KeyBuf,
}

// MVCC-aware LeafEntry
pub struct LeafEntry {
    pub mbr: Mbr,
    pub object_id: KeyBuf,
    pub version_chain: VersionChain,  // Track visibility
}
```

**Pros**:
- Fine-grained version control
- Efficient visibility checking
- Supports concurrent updates

**Cons**:
- Increases node size
- More complex serialization
- May reduce fanout

#### Approach 2: Separate Version Tracking (Alternative)
Keep index structure unchanged, track versions separately:

```rust
pub struct PagedRTree<FS: FileSystem> {
    // ... existing fields ...
    
    /// Version tracking: object_id -> VersionChain
    versions: RwLock<HashMap<Vec<u8>, VersionChain>>,
}
```

**Pros**:
- Simpler to implement
- Doesn't change node structure
- Easier to add to existing code

**Cons**:
- Extra lookup required
- Separate data structure to maintain
- Potential consistency issues

#### Approach 3: Metadata-Only Versioning (For Blobs)
Blob tables store large objects - version the metadata, not the content:

```rust
pub struct BlobMetadata {
    pub key: Vec<u8>,
    pub size: u64,
    pub page_ids: Vec<PageId>,  // Pages storing blob data
    pub created_by: TransactionId,
    pub commit_lsn: Option<LogSequenceNumber>,
}

// Store version chain of metadata
pub struct BlobEntry {
    pub metadata_chain: VersionChain,  // Serialized BlobMetadata
}
```

**Pros**:
- Efficient for large objects
- Avoids duplicating blob data
- Natural fit for blob storage

**Cons**:
- Requires careful garbage collection
- Orphaned pages if not handled properly

## Implementation Plan

### Phase 1: Prerequisites (MUST DO FIRST)
1. ✅ **Resolve Nanostore-8xz**: Fix PagedRTree interior mutability
2. ✅ **Resolve Nanostore-ckm**: Update transaction commit path
3. ✅ **Test infrastructure**: Ensure MVCC tests work for existing tables

### Phase 2: PagedRTree Integration
1. **Add VersionChain to LeafEntry**:
   ```rust
   pub struct LeafEntry {
       pub mbr: Mbr,
       pub object_id: KeyBuf,
       pub version_chain: VersionChain,
   }
   ```

2. **Update serialization**:
   - Modify `LeafEntry::to_bytes()` to include version chain
   - Modify `LeafEntry::from_bytes()` to deserialize version chain
   - Use postcard for VersionChain serialization

3. **Add transaction-aware methods**:
   ```rust
   impl<FS: FileSystem> PagedRTree<FS> {
       pub fn insert_geometry_tx(
           &self,
           id: &[u8],
           geometry: GeometryRef<'_>,
           tx_id: TransactionId,
       ) -> TableResult<()>;
       
       pub fn delete_geometry_tx(
           &self,
           id: &[u8],
           tx_id: TransactionId,
       ) -> TableResult<()>;
       
       pub fn search_snapshot(
           &self,
           query: GeometryRef<'_>,
           snapshot: &Snapshot,
           limit: usize,
       ) -> TableResult<Vec<GeoHit>>;
       
       pub fn commit_versions(
           &self,
           tx_id: TransactionId,
           commit_lsn: LogSequenceNumber,
       ) -> TableResult<()>;
       
       pub fn vacuum(
           &self,
           min_visible_lsn: LogSequenceNumber,
       ) -> TableResult<usize>;
   }
   ```

4. **Update search operations**:
   - Modify `search_intersects_recursive()` to check version visibility
   - Modify `search_nearest()` to check version visibility
   - Pass `Snapshot` through search methods

5. **Add tests**:
   - Test concurrent geometry inserts
   - Test snapshot isolation for spatial queries
   - Test version vacuuming
   - Test transaction rollback

### Phase 3: PagedHnswVector Integration
Similar approach to PagedRTree:
1. Add VersionChain to vector entries
2. Update serialization
3. Add transaction-aware methods
4. Update search operations for visibility
5. Add tests

### Phase 4: PagedFullTextIndex Integration
1. Add VersionChain to posting list entries
2. Update inverted index operations
3. Add transaction-aware document indexing
4. Update search to respect visibility
5. Add tests

### Phase 5: Blob Tables Integration
1. **MemoryBlob**: Add `HashMap<Vec<u8>, VersionChain>` for metadata
2. **PagedBlob**: Add VersionChain to blob metadata pages
3. **FileBlob**: Add metadata file with version tracking
4. Implement garbage collection for orphaned blob pages
5. Add tests

## Testing Strategy

### Unit Tests
For each table, add tests for:
- Version chain creation and prepending
- Visibility checking with snapshots
- Commit and rollback
- Vacuum with active snapshots

### Integration Tests
- Cross-table transactions (e.g., update geometry and blob in same transaction)
- Concurrent access patterns
- Long-running transactions
- Recovery after crash

### Performance Tests
- Measure overhead of version chains
- Compare query performance with/without MVCC
- Measure memory usage with long version chains
- Benchmark vacuum performance

## Migration Path

### Backward Compatibility
- Add version field to file format
- Support reading old format (pre-MVCC)
- Automatic migration on first write
- Document migration process

### Rollout Strategy
1. Implement and test each table independently
2. Add feature flag for MVCC in specialty tables
3. Enable by default after thorough testing
4. Provide migration tool for existing databases

## Performance Considerations

### Memory Overhead
- Version chains increase memory usage
- Estimate: ~50-100 bytes per version
- Mitigation: Aggressive vacuum policy

### Query Performance
- Visibility checking adds overhead
- Estimate: 5-10% slowdown for queries
- Mitigation: Cache visibility results

### Storage Overhead
- Version chains increase page size
- May reduce node fanout
- Mitigation: Larger page sizes for specialty tables

## Open Questions

1. **Should we version the index structure itself or just the entries?**
   - Current plan: Version entries only
   - Alternative: Version entire nodes (more complex)

2. **How to handle bulk operations (e.g., bulk_load for R-Tree)?**
   - Option 1: Treat as single transaction
   - Option 2: Create versions for each entry
   - Recommendation: Option 1 for performance

3. **What's the vacuum policy for specialty tables?**
   - Same as regular tables?
   - More aggressive due to larger entries?
   - Recommendation: Configurable per table type

4. **How to handle index rebuilds?**
   - Preserve version history?
   - Start fresh?
   - Recommendation: Start fresh, document as breaking change

## Dependencies

### Blocked By
- Nanostore-8xz: Fix GeoSpatial apply during transaction commit (Priority 1)
- Nanostore-ckm: Update transaction commit path (Priority 2)

### Blocks
- Nanostore-6ij: Add comprehensive MVCC integration tests (Priority 2)
- Full MVCC support across all table types

## Timeline Estimate

Assuming prerequisites are resolved:

- **Phase 1 (Prerequisites)**: 2-3 days
- **Phase 2 (PagedRTree)**: 3-4 days
- **Phase 3 (PagedHnswVector)**: 2-3 days
- **Phase 4 (PagedFullTextIndex)**: 2-3 days
- **Phase 5 (Blob Tables)**: 2-3 days
- **Testing & Documentation**: 2-3 days

**Total**: 13-19 days

## References

- `src/txn/version.rs` - VersionChain implementation
- `src/table/timeseries/bucket.rs` - Reference implementation
- `src/table/graph/memory.rs` - Reference implementation
- `docs/MVCC_VERSION_CHAIN_INTEGRATION.md` - Overall MVCC plan
- `docs/SPECIALTY_TABLE_TRANSACTIONS.md` - Transaction support for specialty tables

---

*Created: 2026-05-16*
*Issue: Nanostore-302*
*Status: Planning - Blocked by Nanostore-8xz, Nanostore-ckm*