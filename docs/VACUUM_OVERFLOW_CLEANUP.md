# Vacuum Overflow Page Cleanup Implementation

## Overview

Implemented proper overflow page cleanup when `vacuum()` removes external values. Previously, `vacuum()` returned `Vec<ValueRef>` but callers didn't free the associated overflow pages, leading to storage leaks.

## Changes Made

### 1. Pager Layer (`src/pager/pagefile.rs`)

Added two new methods to handle ValueRef cleanup:

#### `free_value_ref(&self, value_ref: &ValueRef) -> PagerResult<usize>`
- Frees overflow pages for a single ValueRef
- Handles all three ValueRef variants:
  - `Inline`: No-op (returns 0)
  - `SinglePage`: Frees single overflow page
  - `OverflowChain`: Frees entire chain via existing `free_overflow_chain()`
- Returns number of pages freed
- Includes metric: `pager.vacuum_pages_freed`

#### `free_value_refs(&self, value_refs: &[ValueRef]) -> PagerResult<usize>`
- Batch operation for multiple ValueRefs
- Continues on error, attempts to free all refs
- Returns total pages freed or first error encountered
- Includes metrics:
  - `pager.vacuum_batch_freed`
  - `pager.vacuum_batch_size`
  - `pager.vacuum_total_pages_freed`

### 2. Table Layer Updates

Updated all paged table implementations to process freed_refs from `vacuum()`:

#### BTree (`src/table/btree/paged.rs`)
- Modified `vacuum_node()` to call `pager.free_value_refs()` for freed refs
- Properly handles PagerError via `?` operator (auto-converts to TableError)

#### RTree (`src/table/rtree/paged.rs`)
- Updated `RTreeEntry::vacuum()` to return tuple `(usize, Vec<ValueRef>)`
- Modified `vacuum_recursive()` to free overflow pages in leaf nodes

#### HNSW (`src/table/hnsw/paged.rs`)
- Updated `HnswNode::vacuum()` to return tuple `(usize, Vec<ValueRef>)`
- Modified main `vacuum()` to free overflow pages after vacuuming each node

#### Blob Tables
- `src/table/blob/file.rs`: Added overflow cleanup in vacuum loop
- `src/table/blob/paged.rs`: Added overflow cleanup in vacuum loop  
- `src/table/blob/memory.rs`: Updated to handle tuple return (no cleanup needed for in-memory)

#### FullText (`src/table/fulltext/posting.rs` & `mod.rs`)
- Updated `PostingEntry::vacuum()` to return tuple
- Updated `PostingList::vacuum()` to aggregate freed_refs
- Main vacuum in `mod.rs` handles tuple (no cleanup for in-memory index)

#### Graph (`src/table/graph/memory.rs`)
- Updated edge chain vacuum calls to handle tuple return
- No cleanup needed (in-memory storage)

### 3. Metrics Added

New metrics for monitoring vacuum overflow cleanup:
- `pager.vacuum_pages_freed`: Counter for individual page frees
- `pager.vacuum_batch_freed`: Counter for batch operations
- `pager.vacuum_batch_size`: Histogram of batch sizes
- `pager.vacuum_total_pages_freed`: Histogram of total pages freed per batch

Existing metrics reused:
- `pager.overflow_chain_freed`: Counter when freeing chains
- `pager.overflow_chain_pages_freed`: Histogram of pages per chain

## Error Handling

- Uses existing `PagerError` type which auto-converts to `TableError` via `From` trait
- Batch operation (`free_value_refs`) continues on error to maximize cleanup
- Logs warnings for individual failures but returns first error

## Testing Considerations

The implementation:
1. Reuses existing `free_overflow_chain()` which is already tested
2. Adds thin wrappers that handle ValueRef variants
3. Integrates into existing vacuum flows

Testing should verify:
- Overflow pages are actually freed after vacuum
- Page count decreases appropriately
- No double-free issues
- Metrics are incremented correctly

## Backward Compatibility

- No breaking changes to public APIs
- Existing vacuum() signatures unchanged at table level
- Internal changes only affect version chain vacuum return type

## Performance Impact

- Minimal: Only adds cleanup for pages that were already being identified
- Batch operation reduces overhead vs individual frees
- Metrics add negligible overhead

## Future Improvements

1. Consider making vacuum return `TableResult<VacuumReport>` with:
   - Versions removed
   - Pages freed
   - Bytes reclaimed
   
2. Add vacuum statistics to table-level metrics

3. Consider async/background cleanup for large vacuum operations

## Files Modified

- `src/pager/pagefile.rs` - Added free_value_ref methods
- `src/table/btree/paged.rs` - Integrated cleanup
- `src/table/rtree/paged.rs` - Integrated cleanup
- `src/table/rtree/node.rs` - Updated vacuum signature
- `src/table/hnsw/paged.rs` - Integrated cleanup
- `src/table/fulltext/posting.rs` - Updated vacuum signatures
- `src/table/fulltext/mod.rs` - Handle tuple return
- `src/table/blob/file.rs` - Integrated cleanup
- `src/table/blob/paged.rs` - Integrated cleanup
- `src/table/blob/memory.rs` - Handle tuple return
- `src/table/graph/memory.rs` - Handle tuple return