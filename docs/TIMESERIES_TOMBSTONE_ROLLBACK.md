# Time Series Tombstone-Based Rollback Implementation

## Overview

This document describes the implementation of tombstone-based undo for time series operations, enabling proper transaction rollback for append-only time series data.

## Problem Statement

Time series tables are append-only by design, making traditional undo operations (deleting appended data) problematic. When a transaction that appended time series points is rolled back, those points should not be visible to subsequent transactions, but physically removing them would violate the append-only constraint and could cause data corruption.

## Solution: Tombstone-Based Undo

Instead of physically removing rolled-back points, we mark them with tombstones that indicate they should be filtered out during scans. This approach:

1. Preserves the append-only nature of time series data
2. Allows efficient rollback without data deletion
3. Supports MVCC visibility semantics
4. Enables cleanup during vacuum operations

## Architecture

### Tombstone Structure

```rust
pub struct TimeSeriesTombstone {
    /// Series key
    pub series_key: Vec<u8>,
    /// Timestamp of the tombstoned point
    pub timestamp: i64,
    /// Transaction ID that created this tombstone
    pub tombstone_tx_id: TransactionId,
    /// LSN when the tombstone was created
    pub tombstone_lsn: Option<LogSequenceNumber>,
}
```

### Storage

Tombstones are stored in the `TimeBucket` structure alongside data points:

```rust
pub struct TimeBucket<FS: FileSystem> {
    // ... existing fields ...
    
    /// Tombstones for rolled-back points: (timestamp, tombstone)
    tombstones: BTreeMap<i64, TimeSeriesTombstone>,
}
```

Tombstones are:
- Serialized with bucket data during `to_bytes()`
- Persisted to disk via the pager when buckets are flushed
- Deserialized during `from_bytes()` when buckets are loaded
- Stored in the same page as the bucket's data points

### Visibility Rules

A time series point is visible to a transaction if:
1. The point's version is visible according to MVCC rules (commit LSN ≤ snapshot LSN)
2. AND the point is NOT tombstoned for that snapshot

A tombstone is visible (hides the point) if:
- The tombstone has been committed (`tombstone_lsn.is_some()`)
- AND the tombstone's LSN ≤ snapshot LSN

## Implementation Details

### 1. Tombstone Creation (Rollback)

When a transaction is rolled back, the `execute_undo()` method creates tombstones:

```rust
UndoOperation::RemoveTimeSeriesPoint {
    object_id,
    series_key,
    timestamp,
    value_key: _,
} => {
    // Add a tombstone to mark this point as rolled back
    if let Some(engine) = self.engine_registry.get(*object_id) {
        if let TableEngineInstance::TimeSeriesTable(timeseries) = &engine {
            timeseries
                .add_tombstone(series_key, *timestamp, self.txn_id)
                .map_err(|e| TransactionError::Other(...))?;
        }
    }
}
```

### 2. Tombstone Filtering (Scans)

All scan operations filter out tombstoned points:

```rust
pub fn range<'a>(
    &'a self,
    start: i64,
    end: i64,
    snapshot: &'a Snapshot,
) -> impl Iterator<Item = (i64, Vec<u8>)> + 'a {
    self.points
        .range(start..end)
        .filter_map(move |(ts, chain)| {
            // Filter out tombstoned points
            if self.is_tombstoned(*ts, snapshot) {
                return None;
            }
            chain
                .find_visible_version(snapshot)
                .map(|v| (*ts, v.to_vec()))
        })
}
```

This filtering is applied to:
- `range()` - Range scans
- `iter()` - Full bucket iteration
- `latest_before()` - Latest point queries
- `get()` - Point lookups

### 3. Tombstone Commit

When a transaction commits, tombstones are committed alongside version chains:

```rust
pub fn commit_versions(
    &self,
    tx_id: TransactionId,
    commit_lsn: LogSequenceNumber,
) -> TableResult<()> {
    let mut state = self.state.write().unwrap();

    for manager in state.series.values_mut() {
        for bucket in manager.buckets.values_mut() {
            bucket.commit_versions(tx_id, commit_lsn);
            bucket.commit_tombstones(tx_id, commit_lsn);  // Commit tombstones
        }
    }

    Ok(())
}
```

### 4. Tombstone Cleanup (Vacuum)

Tombstones are removed during vacuum when they're no longer needed:

```rust
pub fn vacuum(&mut self, min_visible_lsn: LogSequenceNumber) -> usize {
    let mut removed = 0;
    
    // Vacuum version chains
    for chain in self.points.values_mut() {
        removed += chain.vacuum(min_visible_lsn);
    }
    
    // Remove tombstones that are no longer needed
    let tombstones_to_remove: Vec<i64> = self.tombstones
        .iter()
        .filter(|(_, tombstone)| {
            if let Some(lsn) = tombstone.tombstone_lsn {
                lsn < min_visible_lsn
            } else {
                false
            }
        })
        .map(|(ts, _)| *ts)
        .collect();
    
    for ts in tombstones_to_remove {
        self.tombstones.remove(&ts);
        removed += 1;
    }
    
    if removed > 0 {
        self.dirty = true;
    }
    removed
}
```

## Serialization Format

Tombstones are serialized after data points in the bucket format:

```
[Bucket Header]
[Point Count: u32]
[Points: (timestamp: i64, chain_len: u32, chain_data: bytes)*]
[Tombstone Count: u32]                    // NEW
[Tombstones: (tombstone_len: u32, tombstone_data: bytes)*]  // NEW
```

The format is backward compatible - older buckets without tombstones will have no data after the points section, and deserialization gracefully handles missing tombstone data.

## Transaction Flow

### Successful Commit

1. Transaction appends points to time series
2. Points are added to buckets with transaction ID
3. Transaction commits
4. Version chains are committed with commit LSN
5. Points become visible to new transactions

### Rollback

1. Transaction appends points to time series
2. Points are added to buckets with transaction ID
3. Transaction fails and rolls back
4. `execute_undo()` creates tombstones for each appended point
5. Tombstones are committed with rollback LSN
6. Points are filtered out during scans (not visible)
7. Eventually, vacuum removes old tombstones

## Performance Considerations

### Memory Overhead

- Each tombstone: ~40-60 bytes (series_key + timestamp + metadata)
- Tombstones are stored per-bucket, not globally
- Vacuum removes old tombstones, limiting growth

### Scan Performance

- Tombstone check is O(log n) per point (BTreeMap lookup)
- Minimal impact on scan performance
- Tombstones are typically sparse (only for rolled-back transactions)

### Disk Space

- Tombstones are persisted with bucket data
- Adds ~50-100 bytes per rolled-back point
- Vacuum reclaims space when tombstones are no longer needed

## Limitations

1. **Append-Only Constraint**: Tombstones don't physically remove data, only hide it
2. **Vacuum Required**: Old tombstones accumulate until vacuum runs
3. **No Point Updates**: Time series points cannot be updated, only appended
4. **Timestamp Uniqueness**: Multiple versions at same timestamp use version chains

## Future Enhancements

1. **Automatic Vacuum**: Trigger vacuum when tombstone count exceeds threshold
2. **Tombstone Compression**: Compress tombstone storage for large rollbacks
3. **Range Tombstones**: Support tombstoning entire time ranges efficiently
4. **Tombstone Statistics**: Track tombstone metrics for monitoring

## Testing

Comprehensive tests should cover:

1. **Basic Rollback**: Single transaction rollback
2. **Interleaved Commits**: Mix of committed and rolled-back points
3. **Multiple Series**: Rollback across multiple time series
4. **Same Timestamp**: Rolling back points that overwrite existing data
5. **Latest Before**: Queries respect tombstones
6. **Aggregations**: Sum/avg/count respect tombstones
7. **Persistence**: Tombstones survive database restart
8. **Vacuum**: Old tombstones are cleaned up

## Related Documentation

- [`docs/TWO_PHASE_COMMIT_IMPLEMENTATION.md`](TWO_PHASE_COMMIT_IMPLEMENTATION.md) - Overall undo mechanism
- [`docs/SPECIALTY_TABLE_TRANSACTIONS.md`](SPECIALTY_TABLE_TRANSACTIONS.md) - Specialty table transaction support
- [`docs/MVCC_VERSION_CHAIN_INTEGRATION.md`](MVCC_VERSION_CHAIN_INTEGRATION.md) - MVCC version chains

## Files Modified

1. [`src/table/timeseries/bucket.rs`](../src/table/timeseries/bucket.rs)
   - Added `TimeSeriesTombstone` structure
   - Added `tombstones` field to `TimeBucket`
   - Implemented `add_tombstone()`, `commit_tombstones()`, `is_tombstoned()`
   - Updated scan methods to filter tombstoned points
   - Updated serialization to include tombstones
   - Enhanced `vacuum()` to remove old tombstones

2. [`src/table/timeseries/mod.rs`](../src/table/timeseries/mod.rs)
   - Added `add_tombstone()` method to `TimeSeriesTable`
   - Updated `commit_versions()` to commit tombstones

3. [`src/txn/transaction.rs`](../src/txn/transaction.rs)
   - Implemented `UndoOperation::RemoveTimeSeriesPoint` to create tombstones
   - Integrated with transaction rollback mechanism

## Conclusion

The tombstone-based undo mechanism provides a robust solution for time series transaction rollback while preserving the append-only nature of time series data. The implementation is efficient, MVCC-compatible, and supports proper cleanup through vacuum operations.

---

*Made with Bob*