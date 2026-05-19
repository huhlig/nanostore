# Vacuum/Garbage Collection Implementation

## Overview

This document describes the vacuum/garbage collection system for removing obsolete MVCC version chains in Nanostore.

## Architecture

### Core Components

1. **VersionChain::vacuum()** (src/txn/version.rs)
   - Already implemented and working correctly
   - Removes obsolete committed versions older than min_visible_lsn
   - Retains one old version as a base for efficiency
   - Returns count of removed versions

2. **Database::min_visible_lsn()** (src/kvdb.rs)
   - Computes minimum LSN across all active snapshots
   - This is the watermark below which versions can be safely removed
   - Returns None if no active snapshots (conservative: keep all versions)

3. **Database::vacuum_table()** (src/kvdb.rs)
   - Vacuums a specific table
   - Gets min_visible_lsn and delegates to engine registry
   - Returns count of versions removed

4. **Database::vacuum_all()** (src/kvdb.rs)
   - Convenience method to vacuum all tables
   - Skips tables that don't support vacuuming
   - Returns map of table_id -> versions_removed

5. **TableEngineRegistry::vacuum_table()** (src/table.rs)
   - Dispatcher that calls appropriate engine's vacuum method
   - Handles engines that don't support vacuum (AppendLog, Bloom, etc.)

## Supported Engines

The following engines support vacuum (once implementations are added):

- ✅ MemoryBTree
- ✅ PagedBTree  
- ✅ MemoryHashTable
- ✅ MemoryART
- ✅ LsmTree (memtable + sstable)
- ✅ MemoryGraphTable
- ✅ TimeSeriesTable

Engines that don't support vacuum:
- ❌ AppendLog (append-only by design)
- ❌ PagedBloomFilter (no version chains)
- ❌ PagedHnswVector (not yet implemented)
- ❌ PagedRTree (not yet implemented)
- ❌ PagedFullTextIndex (not yet implemented)
- ❌ MemoryBlob (no version chains)

## Usage

### Manual Vacuum

```rust
// Vacuum a specific table
let removed = db.vacuum_table(table_id)?;
println!("Removed {} obsolete versions", removed);

// Vacuum all tables
let results = db.vacuum_all()?;
for (table_id, removed) in results {
    println!("Table {}: removed {} versions", table_id, removed);
}
```

### With Snapshots

```rust
// Create a snapshot to pin versions
let snapshot = db.create_snapshot("backup")?;

// Vacuum will preserve versions visible to this snapshot
db.vacuum_all()?;

// Release snapshot when done
db.release_snapshot(snapshot.id)?;

// Now vacuum can remove more versions
db.vacuum_all()?;
```

## Implementation Status

### Completed (Nanostore-5fv) ✅ CLOSED
- ✅ VersionChain::vacuum() method (already existed)
- ✅ Database::min_visible_lsn() for watermark computation
- ✅ Database::vacuum_table() and vacuum_all() APIs
- ✅ TableEngineRegistry::vacuum_table() dispatcher
- ✅ Infrastructure and design complete
- ✅ vacuum() methods implemented in all table engines (Nanostore-cod)
- ✅ Basic tests passing (version_chain_tests, timeseries_mvcc_tests, memtable tests)

### Remaining Work

1. **Background vacuum task** (Nanostore-mkk) - OPEN
   - Periodic background task in Database
   - Configurable vacuum interval
   - Manual trigger API
   - Metrics tracking:
     - Total versions removed per run
     - Average version chain length before/after
     - Time taken for vacuum operations
     - Memory freed by vacuum

3. **Comprehensive tests** (Nanostore-914)
   - Basic vacuum functionality
   - Watermark respect
   - Base version preservation
   - Active snapshot handling
   - Concurrent operations
   - Metrics accuracy
   - Different table engines

## Design Decisions

### Watermark-Based Approach

We use a watermark-based approach similar to PostgreSQL:
- Compute minimum LSN across all active snapshots
- This is the "horizon" below which versions are invisible
- Versions with commit_lsn < min_visible_lsn can be removed
- Always keep one old version as a base for efficiency

### Conservative Default

When no snapshots are active, we use the current LSN as the watermark. This is conservative but safe - it keeps all versions that might be visible to future snapshots.

### Per-Table Granularity

Vacuum operates at table granularity, not database-wide. This allows:
- Selective vacuuming of hot tables
- Better concurrency (lock one table at a time)
- Easier progress tracking and metrics

### Engine-Specific Implementation

Each engine implements vacuum differently based on its data structures:
- BTree: Iterate through tree nodes
- Hash: Iterate through hash buckets
- LSM: Vacuum memtable + trigger compaction for SSTables
- Graph: Vacuum both outgoing and incoming edge indexes
- TimeSeries: Vacuum each bucket's point map

## Future Enhancements

1. **Incremental Vacuum**
   - Vacuum a subset of keys per call
   - Resume from last position
   - Better for large tables

2. **Vacuum Scheduling**
   - Smart scheduling based on workload
   - Avoid vacuum during peak hours
   - Prioritize tables with long version chains

3. **Vacuum Statistics**
   - Track vacuum history per table
   - Estimate next vacuum time
   - Alert on excessive version chain growth

4. **Concurrent Vacuum**
   - Allow reads during vacuum
   - Use MVCC for vacuum operations themselves
   - Minimize lock contention

## References

- PostgreSQL VACUUM: https://www.postgresql.org/docs/current/routine-vacuuming.html
- MVCC in PostgreSQL: https://www.postgresql.org/docs/current/mvcc.html
- src/txn/version.rs - VersionChain implementation
- src/kvdb.rs - Database vacuum APIs
- src/table.rs - TableEngineRegistry dispatcher

---
Made with Bob