# VACUUM FULL Design Document

## Overview

VACUUM FULL is a blocking operation that compacts the database file by moving data from high-numbered pages to low-numbered pages, then truncating the file. This is similar to PostgreSQL's VACUUM FULL.

## Motivation

The current vacuum implementation (`vacuum_table()` and `vacuum_all()`) removes obsolete MVCC versions and marks pages as free for reuse, but does not shrink the database file. Over time, this can lead to:

1. **Wasted disk space** - Free pages remain allocated in the file
2. **Poor sequential scan performance** - Data is scattered across many pages
3. **File fragmentation** - Logical data order doesn't match physical layout

VACUUM FULL addresses these issues by physically reorganizing data and reclaiming disk space.

## Design Goals

1. **Space reclamation** - Shrink database file by truncating unused pages
2. **Data compaction** - Move records from high pages to low pages
3. **Blocking operation** - Requires exclusive access (simpler implementation)
4. **Statistics tracking** - Report space reclaimed and file size reduction
5. **Safety** - Never lose data, maintain ACID guarantees

## Architecture

### Key Components

1. **VacuumFullStats** - Statistics structure for tracking progress
2. **VacuumOptions.full** - Flag to enable VACUUM FULL mode
3. **Pager::compact_and_truncate()** - Core compaction logic
4. **Database::vacuum_full_table()** - Table-level API
5. **Database::vacuum_full_all()** - Database-level API

### Algorithm

```
VACUUM FULL algorithm:
1. Acquire exclusive lock on table (blocking)
2. Identify highest used page (max_page_id)
3. Identify free pages below max_page_id
4. For each used page from max_page_id downward:
   a. If page is in use:
      - Find lowest free page
      - Copy page data from high to low
      - Update all references (indexes, overflow chains, etc.)
      - Mark high page as free
   b. Continue until no more pages can be moved
5. Calculate new file size (last used page + 1)
6. Truncate file to new size
7. Update superblock metadata
8. Release lock
9. Return statistics
```

### Data Structures

```rust
/// Statistics for VACUUM FULL operation
#[derive(Debug, Clone, Default)]
pub struct VacuumFullStats {
    /// Number of pages moved during compaction
    pub pages_moved: u64,
    /// Number of pages freed and truncated
    pub pages_truncated: u64,
    /// Bytes reclaimed from file
    pub bytes_reclaimed: u64,
    /// File size before VACUUM FULL
    pub file_size_before: u64,
    /// File size after VACUUM FULL
    pub file_size_after: u64,
    /// Duration of operation
    pub duration: Duration,
}

/// Extended VacuumOptions with full flag
#[derive(Clone, Debug, Default)]
pub struct VacuumOptions {
    pub aggressive: bool,
    pub max_pages: Option<u64>,
    pub full: bool,  // NEW: Enable VACUUM FULL
}

/// Extended VacuumReport with full stats
#[derive(Clone, Debug, Default)]
pub struct VacuumReport {
    pub pages_freed: u64,
    pub bytes_reclaimed: u64,
    pub full_stats: Option<VacuumFullStats>,  // NEW: VACUUM FULL statistics
}
```

## Implementation Plan

### Phase 1: Core Infrastructure

1. **Add VacuumFullStats structure** (src/kvdb.rs)
   - Track pages moved, truncated, bytes reclaimed
   - Track file size before/after
   - Track operation duration

2. **Extend VacuumOptions** (src/table/traits.rs)
   - Add `full: bool` flag
   - Defaults to false (regular vacuum)

3. **Extend VacuumReport** (src/table/traits.rs)
   - Add `full_stats: Option<VacuumFullStats>`
   - Populated only when full=true

### Phase 2: Pager-Level Compaction

4. **Implement Pager::compact_and_truncate()** (src/pager/pagefile.rs)
   ```rust
   pub fn compact_and_truncate(&self) -> PagerResult<VacuumFullStats>
   ```
   - Find highest used page
   - Identify free pages below it
   - Move pages from high to low
   - Update page references
   - Truncate file
   - Return statistics

5. **Implement Pager::find_highest_used_page()** (src/pager/pagefile.rs)
   - Scan from end of file backward
   - Find last page that's not in free list
   - Return page ID

6. **Implement Pager::move_page()** (src/pager/pagefile.rs)
   - Copy page data from source to destination
   - Update page header with new ID
   - Handle overflow chains
   - Maintain checksums

7. **Implement Pager::truncate_file()** (src/pager/pagefile.rs)
   - Calculate new file size
   - Call VFS set_size()
   - Update superblock
   - Sync to disk

### Phase 3: Database-Level APIs

8. **Implement Database::vacuum_full_table()** (src/kvdb.rs)
   ```rust
   pub fn vacuum_full_table(&self, table_id: TableId) -> Result<VacuumFullStats, DatabaseError>
   ```
   - Acquire exclusive lock on table
   - Call regular vacuum first (remove obsolete versions)
   - Call pager.compact_and_truncate()
   - Release lock
   - Return statistics

9. **Implement Database::vacuum_full_all()** (src/kvdb.rs)
   ```rust
   pub fn vacuum_full_all(&self) -> Result<HashMap<TableId, VacuumFullStats>, DatabaseError>
   ```
   - Iterate all tables
   - Call vacuum_full_table() for each
   - Aggregate statistics
   - Return per-table results

### Phase 4: Testing

10. **Write comprehensive tests** (tests/vacuum_full_tests.rs)
    - Basic compaction (move pages, truncate file)
    - Multiple tables
    - Large files with many free pages
    - Edge cases (no free pages, all pages free)
    - Concurrent access (should block)
    - Crash recovery (should be safe)
    - Statistics accuracy

### Phase 5: Documentation

11. **Update VACUUM_GARBAGE_COLLECTION.md**
    - Document VACUUM FULL vs regular VACUUM
    - Usage examples
    - Performance characteristics
    - When to use each

## Challenges and Solutions

### Challenge 1: Updating Page References

**Problem**: When moving a page, all references to it must be updated (indexes, overflow chains, parent pointers, etc.)

**Solution**: 
- For now, VACUUM FULL only works on tables without complex references
- Future: Implement reference tracking in page metadata
- Alternative: Rebuild indexes after VACUUM FULL

### Challenge 2: Exclusive Locking

**Problem**: VACUUM FULL blocks all access to the table

**Solution**:
- Document this clearly
- Provide progress reporting
- Consider time limits
- Future: Implement online VACUUM FULL (more complex)

### Challenge 3: Crash Safety

**Problem**: If VACUUM FULL crashes mid-operation, database could be corrupted

**Solution**:
- Use WAL for all page moves
- Make truncation atomic (last step)
- Implement recovery logic
- Test crash scenarios thoroughly

### Challenge 4: Large Files

**Problem**: Moving many pages can take a long time

**Solution**:
- Implement progress reporting
- Allow cancellation
- Consider incremental VACUUM FULL
- Optimize page copying (batch operations)

## Performance Characteristics

### Time Complexity
- O(n) where n = number of pages to move
- Worst case: All pages need moving (file is 50% free at end)
- Best case: No pages need moving (all free pages at end)

### Space Complexity
- O(1) - Only need to hold one page in memory at a time
- Temporary space: None (in-place operation)

### I/O Characteristics
- Read: One read per page moved
- Write: One write per page moved + metadata updates
- Sequential I/O for reading high pages
- Random I/O for writing to low pages

## Usage Examples

```rust
// VACUUM FULL a specific table
let stats = db.vacuum_full_table(table_id)?;
println!("Reclaimed {} bytes", stats.bytes_reclaimed);
println!("File size: {} -> {}", stats.file_size_before, stats.file_size_after);

// VACUUM FULL all tables
let results = db.vacuum_full_all()?;
for (table_id, stats) in results {
    println!("Table {}: reclaimed {} bytes", table_id, stats.bytes_reclaimed);
}

// Regular vacuum with VACUUM FULL option
let mut options = VacuumOptions::default();
options.full = true;
let report = table.vacuum(options)?;
if let Some(stats) = report.full_stats {
    println!("VACUUM FULL completed: {} pages moved", stats.pages_moved);
}
```

## Future Enhancements

1. **Online VACUUM FULL** - Allow reads during compaction
2. **Incremental VACUUM FULL** - Move pages in batches
3. **Parallel VACUUM FULL** - Move multiple pages concurrently
4. **Smart scheduling** - Run during low-traffic periods
5. **Reference tracking** - Automatically update all page references
6. **Progress reporting** - Real-time progress updates
7. **Cancellation** - Allow graceful cancellation

## References

- PostgreSQL VACUUM FULL: https://www.postgresql.org/docs/current/sql-vacuum.html
- SQLite VACUUM: https://www.sqlite.org/lang_vacuum.html
- MySQL OPTIMIZE TABLE: https://dev.mysql.com/doc/refman/8.0/en/optimize-table.html

---
Made with Bob