# Two-Phase Commit Implementation for Transaction Rollback

## Problem Statement

Issue `nanokv-igt`: If transaction commit fails partway through applying writes to storage engines, there is no undo mechanism to roll back the partial changes. The WAL has already recorded the COMMIT record, but some engines may not have received their writes.

## Solution: Two-Phase Commit with Undo Log

### Phase 1: PREPARE
1. Write WAL PREPARE record
2. Collect undo information for all operations
3. Validate that all operations can succeed (if possible)

### Phase 2: COMMIT/APPLY
1. Write WAL COMMIT record
2. Apply changes to all engines sequentially
3. If any apply fails:
   - Execute undo operations in reverse order
   - Write WAL ROLLBACK record
   - Return error to caller
4. Commit version chains
5. Release locks

## Implementation Details

### Undo Operations

Each type of operation needs corresponding undo logic:

- **Put operations**: Store old value (if any) to restore on failure
- **Delete operations**: Store old value to restore on failure
- **Bloom inserts**: Use tombstone-based undo (see [`BLOOM_FILTER_TOMBSTONE_ROLLBACK.md`](BLOOM_FILTER_TOMBSTONE_ROLLBACK.md))
- **Graph operations**: Store reverse operations
- **Time series**: Use tombstone-based undo (see [`TIMESERIES_TOMBSTONE_ROLLBACK.md`](TIMESERIES_TOMBSTONE_ROLLBACK.md))
- **Vector operations**: Store reverse operations
- **Geospatial operations**: Store reverse operations
- **Full-text operations**: Store reverse operations

### Error Handling

- If PREPARE phase fails: No changes applied, safe to abort
- If COMMIT phase fails: Execute undo operations, write ROLLBACK record
- Undo operations themselves must be robust and not fail

### Recovery

During WAL recovery:
- PREPARE without COMMIT/ROLLBACK: Transaction was interrupted, should be rolled back
- COMMIT without version chain commits: Re-apply commit logic
- ROLLBACK: Transaction was properly rolled back, no action needed

## Implementation Status

### Completed ✅

1. **WAL Infrastructure**
   - Added `RecordType::Prepare` enum variant in `src/wal/record.rs`
   - Implemented `write_prepare()` method in `src/wal/writer.rs`
   - Implemented `write_rollback()` method in `src/wal/writer.rs`
   - Updated WAL recovery to handle PREPARE records

2. **Transaction Infrastructure**
   - Created `UndoOperation` enum with variants for all table types in `src/txn/transaction.rs`
   - Created `UndoLog` structure to collect undo operations
   - Implemented `execute_undo()` method to handle all undo operation variants
   - Modified `Transaction::commit()` to use two-phase commit:
     - **Phase 1 (PREPARE)**: Collects undo information for all operations
     - **Phase 2 (COMMIT/APPLY)**: Applies changes with rollback on failure

3. **Error Handling**
   - If any operation fails during Phase 2, undo operations are executed in reverse order
   - ROLLBACK record is written to WAL on failure
   - Transaction state transitions to Aborted
   - All locks are released

## Files Modified

1. `src/wal/record.rs` - Added PREPARE record type
2. `src/wal/writer.rs` - Added write_prepare() and write_rollback() methods
3. `src/txn/transaction.rs` - Added undo mechanism and two-phase commit logic

## Testing Strategy

1. Test commit failure scenarios for each table type
2. Test undo operations work correctly
3. Test WAL recovery with interrupted transactions
4. Test performance impact of undo log collection

## Limitations

- Bloom filters use tombstone-based undo (see [`BLOOM_FILTER_TOMBSTONE_ROLLBACK.md`](BLOOM_FILTER_TOMBSTONE_ROLLBACK.md))
- Time series use tombstone-based undo (see [`TIMESERIES_TOMBSTONE_ROLLBACK.md`](TIMESERIES_TOMBSTONE_ROLLBACK.md))
- Undo operations add overhead to commit process
- More complex recovery logic required
- Tombstones consume memory until vacuumed

## Time Series Rollback

Time series operations now support rollback through a tombstone-based mechanism:

- Rolled-back points are marked with tombstones instead of being deleted
- Tombstones are persisted with bucket data in the pager
- Scan operations filter out tombstoned points
- Vacuum operations clean up old tombstones

See [`TIMESERIES_TOMBSTONE_ROLLBACK.md`](TIMESERIES_TOMBSTONE_ROLLBACK.md) for complete details.