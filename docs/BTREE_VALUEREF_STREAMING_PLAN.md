# BTree ValueRef Streaming Implementation Plan

## Issue: Nanostore-72g - Implement true streaming with ValueRef for BTree

## Current State

The PagedBTree currently uses the default streaming implementations that load full values into memory:

1. **Line 1738-1746**: `get_stream()` uses default implementation
   - Calls `get()` which loads full value
   - Wraps result in `SliceValueStream`
   - No true streaming from overflow pages

2. **Line 1831-1837**: `put_stream()` buffers entire stream
   - Reads full stream into memory buffer
   - Stores in `pending_changes` as `Vec<u8>`
   - Allocates overflow chain during flush

## Problem

For large values (>1MB), this approach:
- Consumes excessive memory
- Causes allocation pressure
- Limits concurrent operations
- Defeats the purpose of streaming API

## Architecture Requirements

### 1. VersionChain Modification

Current structure stores full value:
```rust
pub struct VersionChain {
    pub value: Vec<u8>,  // ❌ Full value in memory
    pub created_by: TransactionId,
    pub commit_lsn: Option<LogSequenceNumber>,
    pub prev_version: Option<Box<VersionChain>>,
}
```

Proposed structure with ValueRef:
```rust
pub struct VersionChain {
    pub value_ref: ValueRef,  // ✅ Reference to value storage
    pub created_by: TransactionId,
    pub commit_lsn: Option<LogSequenceNumber>,
    pub prev_version: Option<Box<VersionChain>>,
}

// ValueRef already exists in types.rs
pub enum ValueRef {
    Inline,  // Small values stored directly
    SinglePage { page_id: u32, offset: u16, length: u32 },
    OverflowChain { first_page_id: u32, total_length: u64, page_count: u32 },
}
```

### 2. Impact Analysis

This change affects:

#### Core MVCC System
- `src/txn/version.rs` - VersionChain structure
- `find_visible_version()` - Must handle ValueRef
- `vacuum()` - Must free overflow pages
- Serialization/deserialization of version chains

#### BTree Implementation
- `src/table/btree/paged.rs` - Node serialization
- Leaf node entries store VersionChain
- Must encode/decode ValueRef in pages
- Page size calculations change

#### Other Table Types
- LSM Tree (`src/table/lsm/`) - Also uses VersionChain
- Any other MVCC-enabled tables
- All must be updated consistently

#### Pager Integration
- Must track overflow page ownership
- Garbage collection of orphaned pages
- Reference counting for shared pages (future)

### 3. Implementation Phases

#### Phase 1: VersionChain Refactoring (High Risk)
1. Add `value_ref` field to VersionChain
2. Update all VersionChain methods
3. Modify serialization format
4. Update vacuum to free overflow pages
5. **Risk**: Breaks all existing MVCC code

#### Phase 2: BTree Integration
1. Update leaf node serialization
2. Implement true `get_stream()` with OverflowChainStream
3. Implement true `put_stream()` with direct overflow allocation
4. Update page size calculations
5. **Risk**: Complex serialization logic

#### Phase 3: LSM Integration
1. Update SSTable format for ValueRef
2. Modify compaction to handle overflow pages
3. Update memtable flush logic
4. **Risk**: Compaction complexity increases

#### Phase 4: Testing & Validation
1. Unit tests for VersionChain with ValueRef
2. Integration tests for large value operations
3. MVCC tests with streaming
4. Performance benchmarks
5. **Risk**: Hard to test all edge cases

## Recommended Approach

Given the scope and risk, I recommend a **phased, incremental approach**:

### Option A: Hybrid VersionChain (Lower Risk)

Keep both inline and external storage:

```rust
pub struct VersionChain {
    pub value: VersionValue,  // Enum for inline or external
    pub created_by: TransactionId,
    pub commit_lsn: Option<LogSequenceNumber>,
    pub prev_version: Option<Box<VersionChain>>,
}

pub enum VersionValue {
    Inline(Vec<u8>),           // Small values
    External(ValueRef),         // Large values
}
```

**Benefits:**
- Backward compatible
- Gradual migration
- Can optimize per-table
- Less risky

**Drawbacks:**
- More complex code
- Two code paths to maintain

### Option B: Full ValueRef Migration (Higher Risk)

Replace `Vec<u8>` with `ValueRef` everywhere:

**Benefits:**
- Cleaner architecture
- Single code path
- Better long-term design

**Drawbacks:**
- High risk of breakage
- All-or-nothing change
- Complex migration

## Recommendation

**Defer this optimization** for the following reasons:

1. **Priority 3**: Marked as "not critical for v1"
2. **High Risk**: Affects core MVCC system
3. **Wide Impact**: Touches many subsystems
4. **Current Workaround**: Default streaming works, just not optimal
5. **Better Timing**: Should be done with comprehensive testing

### Alternative: Document the Pattern

Instead of implementing now, create a clear pattern for when this is needed:

1. Document the hybrid approach in ADR
2. Add detailed TODOs with implementation notes
3. Create tracking issue for v2.0
4. Focus on higher-priority v1 features

## Implementation Checklist (When Ready)

- [ ] Design VersionValue enum (hybrid approach)
- [ ] Update VersionChain structure
- [ ] Modify find_visible_version() for ValueRef
- [ ] Update vacuum() to free overflow pages
- [ ] Add ValueRef serialization to BTree nodes
- [ ] Implement BTree get_stream() with OverflowChainStream
- [ ] Implement BTree put_stream() with direct allocation
- [ ] Update LSM SSTable format
- [ ] Modify LSM compaction for ValueRef
- [ ] Add comprehensive tests
- [ ] Performance benchmarks
- [ ] Update documentation

## Related Issues

- Nanostore-hc5 (closed as duplicate)
- Future: LSM streaming optimization
- Future: Bloom filter streaming (if needed)

## References

- [ValueRef Streaming Architecture](VALUEREF_STREAMING_ARCHITECTURE.md)
- [Streaming API Implementation](STREAMING_API_IMPLEMENTATION.md)
- [ADR-012: Unified Table Architecture](adrs/012-unified-table-architecture.md)
- `src/pager/overflow_stream.rs` - Existing streaming infrastructure
- `src/types.rs` - ValueRef definition