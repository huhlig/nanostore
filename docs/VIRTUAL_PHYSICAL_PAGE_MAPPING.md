# Virtual-to-Physical Page Mapping Design

## Problem Statement

The current `vacuum_pager()` implementation (formerly `VACUUM FULL`) attempts to compact the database file by moving pages from high-numbered positions to low-numbered positions, then truncating the file. However, this approach has a critical flaw:

**When a page is moved, ALL references to that page must be updated**, including:
- B-tree internal node child pointers
- Overflow chain next-page pointers
- Table root page references
- Free list structures
- Any other data structure storing page IDs

This is complex, error-prone, and causes data loss when references are missed.

## Proposed Solution: Virtual-to-Physical Page Mapping

Introduce an **indirection layer** between logical page IDs (used by tables) and physical page IDs (actual file positions).

### Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                        Table Layer                          │
│  (B-trees, LSM, etc. use VIRTUAL page IDs)                 │
└─────────────────────────┬───────────────────────────────────┘
                          │
                          ▼
┌─────────────────────────────────────────────────────────────┐
│                    Page Mapper                              │
│  virtual_page_id → physical_page_id                         │
│  (Indirection layer - persisted in superblock)             │
└─────────────────────────┬───────────────────────────────────┘
                          │
                          ▼
┌─────────────────────────────────────────────────────────────┐
│                      Pager Layer                            │
│  (Reads/writes PHYSICAL pages from/to file)                │
└─────────────────────────────────────────────────────────────┘
```

### Key Components

#### 1. PageMapper Structure

```rust
/// Maps virtual page IDs to physical page IDs
pub struct PageMapper {
    /// Virtual → Physical mapping
    /// Only contains entries for pages that have been remapped
    /// Unmapped pages use identity mapping (virtual == physical)
    mapping: HashMap<PageId, PageId>,
    
    /// Next available virtual page ID
    next_virtual_id: PageId,
    
    /// Dirty flag for persistence
    dirty: bool,
}

impl PageMapper {
    /// Translate virtual page ID to physical page ID
    pub fn translate(&self, virtual_id: PageId) -> PageId {
        self.mapping.get(&virtual_id).copied().unwrap_or(virtual_id)
    }
    
    /// Allocate a new virtual page ID
    pub fn allocate_virtual(&mut self) -> PageId {
        let id = self.next_virtual_id;
        self.next_virtual_id = PageId(id.0 + 1);
        self.dirty = true;
        id
    }
    
    /// Remap a virtual page to a new physical location
    pub fn remap(&mut self, virtual_id: PageId, new_physical_id: PageId) {
        self.mapping.insert(virtual_id, new_physical_id);
        self.dirty = true;
    }
    
    /// Remove a mapping (page freed)
    pub fn unmap(&mut self, virtual_id: PageId) {
        self.mapping.remove(&virtual_id);
        self.dirty = true;
    }
}
```

#### 2. Modified Pager API

```rust
impl<FS: FileSystem> Pager<FS> {
    /// Read a page by VIRTUAL page ID
    pub fn read_page(&self, virtual_id: PageId) -> PagerResult<Page> {
        let physical_id = self.page_mapper.translate(virtual_id);
        self.read_physical_page(physical_id)
    }
    
    /// Write a page by VIRTUAL page ID
    pub fn write_page(&mut self, virtual_id: PageId, page: &Page) -> PagerResult<()> {
        let physical_id = self.page_mapper.translate(virtual_id);
        self.write_physical_page(physical_id, page)
    }
    
    /// Allocate a new page (returns VIRTUAL page ID)
    pub fn allocate_page(&mut self) -> PagerResult<PageId> {
        let virtual_id = self.page_mapper.allocate_virtual();
        let physical_id = self.allocate_physical_page()?;
        if virtual_id != physical_id {
            self.page_mapper.remap(virtual_id, physical_id);
        }
        Ok(virtual_id)
    }
}
```

#### 3. Simplified vacuum_pager()

```rust
impl<FS: FileSystem> Pager<FS> {
    /// Compact the database file by moving pages to lower positions
    pub fn vacuum_pager(&mut self) -> PagerResult<VacuumPagerStats> {
        let mut stats = VacuumPagerStats::default();
        
        // 1. Find highest used PHYSICAL page
        let max_physical = self.find_max_physical_page();
        
        // 2. Find free PHYSICAL pages below max
        let free_pages = self.find_free_physical_pages(max_physical);
        
        // 3. Move pages from high to low positions
        for high_physical in (0..=max_physical).rev() {
            if self.is_physical_page_in_use(high_physical) {
                if let Some(low_physical) = free_pages.pop_front() {
                    // Move physical page
                    self.move_physical_page(high_physical, low_physical)?;
                    
                    // Update mapping for ALL virtual pages pointing to high_physical
                    for (virtual_id, physical_id) in &self.page_mapper.mapping {
                        if *physical_id == high_physical {
                            self.page_mapper.remap(*virtual_id, low_physical);
                        }
                    }
                    
                    stats.pages_moved += 1;
                }
            }
        }
        
        // 4. Calculate new file size
        let new_max_physical = self.find_max_physical_page();
        let new_size = (new_max_physical.0 + 1) * self.page_size;
        
        // 5. Truncate file
        self.file.truncate(new_size)?;
        stats.bytes_reclaimed = stats.file_size_before - new_size;
        
        // 6. Persist mapping
        self.persist_page_mapper()?;
        
        Ok(stats)
    }
}
```

### Benefits

1. **Simplicity**: No need to update table structures during compaction
2. **Safety**: Cannot lose data by missing references
3. **Flexibility**: Can remap pages without touching table data
4. **MVCC-friendly**: Old snapshots can use old mappings
5. **Testability**: Easier to verify correctness
6. **Performance**: Mapping can be cached in memory

### Trade-offs

1. **Indirection overhead**: Extra lookup on every page access
   - Mitigation: Cache hot mappings in memory
   - Mitigation: Use identity mapping by default (no lookup needed)

2. **Mapping persistence**: Must persist mapping table
   - Store in superblock or dedicated pages
   - Compact representation (only non-identity mappings)

3. **Memory overhead**: HashMap in memory
   - Mitigation: Only store non-identity mappings
   - Mitigation: Use compact data structure (e.g., Vec for dense mappings)

### Implementation Plan

#### Phase 1: Core Infrastructure
1. Implement `PageMapper` structure
2. Add mapping persistence to superblock
3. Modify `Pager` to use virtual page IDs
4. Update all table implementations to use virtual IDs

#### Phase 2: Compaction
1. Implement `vacuum_pager()` with page mapping
2. Add tests for page movement
3. Verify data integrity after compaction

#### Phase 3: Optimization
1. Add mapping cache for hot pages
2. Optimize mapping storage format
3. Add metrics for mapping overhead

### Persistence Format

Store mapping in superblock as:

```rust
struct SuperBlock {
    // ... existing fields ...
    
    /// Number of non-identity mappings
    mapping_count: u32,
    
    /// Page containing mapping table (if mapping_count > inline_capacity)
    mapping_page: Option<PageId>,
    
    /// Inline mappings (for small databases)
    inline_mappings: [(PageId, PageId); 16],
}
```

For larger mappings, use dedicated pages:

```
Mapping Page Format:
┌────────────────────────────────────┐
│ Magic: "PMAP"                      │
│ Version: u32                       │
│ Entry Count: u32                   │
│ Next Page: Option<PageId>          │
├────────────────────────────────────┤
│ Entry 0: (virtual, physical)       │
│ Entry 1: (virtual, physical)       │
│ ...                                │
│ Entry N: (virtual, physical)       │
└────────────────────────────────────┘
```

### Migration Strategy

1. **New databases**: Use virtual-physical mapping from start
2. **Existing databases**: 
   - Detect old format in superblock
   - Initialize identity mapping on first open
   - Mark as migrated in superblock

### Testing Strategy

1. **Unit tests**: PageMapper operations
2. **Integration tests**: Pager with mapping
3. **Compaction tests**: vacuum_pager() correctness
4. **Stress tests**: Large databases with many remappings
5. **Migration tests**: Old format → new format

## Comparison with Current Approach

| Aspect | Current (Rewrite References) | Proposed (Virtual Mapping) |
|--------|------------------------------|----------------------------|
| Complexity | High - must find all refs | Low - just update mapping |
| Safety | Error-prone - easy to miss refs | Safe - no refs to update |
| Performance | Slow - must scan all tables | Fast - just move pages |
| MVCC | Difficult - old snapshots broken | Easy - old mappings work |
| Testability | Hard - many edge cases | Easy - simple invariants |
| Maintenance | High - fragile code | Low - clean abstraction |

## Conclusion

Virtual-to-physical page mapping is a **superior architecture** for database file compaction. It trades a small indirection overhead for massive gains in simplicity, safety, and maintainability.

**Recommendation**: Implement this approach instead of trying to fix the current reference-rewriting implementation.

## Implementation Status

### ✅ Phase 1: Core Infrastructure (COMPLETED)
- ✅ Implemented `PageMapper` structure with recovery support
- ✅ Added `virtual_page_id` field to `PageHeader`
- ✅ Modified `Pager` to track virtual-to-physical mappings
- ✅ Implemented mapping persistence in superblock
- ✅ All table implementations use virtual page IDs

### ✅ Phase 2: Compaction (COMPLETED)
- ✅ Implemented `vacuum_pager()` with page mapping
- ✅ Fixed `move_physical_page()` to update page headers correctly
- ✅ Added comprehensive tests for page movement
- ✅ Verified data integrity after compaction across all table types

### ✅ Phase 3: Testing (COMPLETED)
- ✅ Created `tests/vacuum_virtual_physical_mapping_tests.rs`
- ✅ Test: `test_vacuum_table_btree_with_virtual_mapping` - PASSING
- ✅ Test: `test_vacuum_pager_with_virtual_mapping` - PASSING
- ✅ Test: `test_two_level_vacuum_all_table_types` - PASSING
- ✅ Test: `test_vacuum_idempotency` - PASSING
- ✅ Test: `test_virtual_physical_mapping_preservation` - PASSING
- ✅ All existing vacuum tests continue to pass

### 🎯 Results
- **Data integrity**: All tests pass, no data loss
- **Correctness**: Virtual-physical mappings work correctly
- **Idempotency**: Multiple vacuum operations are safe
- **Multi-table**: Works across BTree, LSM, and Hash tables

### 📝 Notes
- The implementation successfully eliminates the data loss bug that occurred when moving pages
- Page headers now correctly track both virtual and physical page IDs
- The mapping layer provides clean separation between logical and physical page management
- All quality gates passed (tests, data integrity verification)

**Status**: ✅ **COMPLETE** - Virtual-to-physical page mapping is fully implemented and tested.