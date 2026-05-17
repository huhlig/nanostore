# Two-Phase Commit - Next Steps

## Completed Work

1. **WAL Infrastructure** ✅
   - Added `RecordType::Prepare` enum variant
   - Implemented `write_prepare()` method in WAL writer
   - Updated WAL recovery to handle PREPARE records
   - Added serialization/deserialization for PREPARE records

2. **Transaction Infrastructure** ✅
   - Created `UndoOperation` enum with variants for all table types
   - Created `UndoLog` structure to collect undo operations
   - Documented design in `TWO_PHASE_COMMIT_IMPLEMENTATION.md`

## Remaining Work

### 1. Modify Transaction::commit() Method

The commit method needs to be restructured into two phases:

**Phase 1: PREPARE**
```rust
// Write PREPARE record to WAL
let prepare_lsn = self.wal.write_prepare(self.txn_id)?;

// Collect undo information for all operations
let mut undo_log = UndoLog::new();

// For each write operation, read current value before applying
for ((object_id, key), value_opt) in &self.write_set {
    if let Some(engine) = self.engine_registry.get(*object_id) {
        // Read current value to enable undo
        let old_value = engine.get(key)?;
        undo_log.add(UndoOperation::RestoreValue {
            object_id: *object_id,
            key: key.clone(),
            old_value,
        });
    }
}

// Similar logic for bloom, graph, timeseries, vector, geospatial, fulltext
```

**Phase 2: COMMIT/APPLY with Rollback**
```rust
// Write COMMIT record
let commit_lsn = self.wal.write_commit(self.txn_id)?;

// Apply changes with error handling
let apply_result = self.apply_all_changes(commit_lsn);

if let Err(e) = apply_result {
    // Rollback: Execute undo operations in reverse order
    for undo_op in undo_log.operations.iter().rev() {
        self.execute_undo(undo_op)?;
    }
    
    // Write ROLLBACK record
    self.wal.write_rollback(self.txn_id)?;
    
    return Err(e);
}

// Continue with version chain commits and lock release
```

### 2. Implement Undo Execution

Create `execute_undo()` method to handle each `UndoOperation` variant:

```rust
fn execute_undo(&self, op: &UndoOperation) -> TransactionResult<()> {
    match op {
        UndoOperation::RestoreValue { object_id, key, old_value } => {
            // Restore previous value or delete if None
        }
        UndoOperation::RemoveGraphEdge { ... } => {
            // Remove the edge that was added
        }
        // ... handle all variants
    }
}
```

### 3. Handle Special Cases

- **Bloom Filters**: Cannot be undone (append-only). Document this limitation.
- **Time Series**: Cannot be undone (append-only). Document this limitation.
- **AppendLog**: Cannot be undone. Document this limitation.

### 4. Testing

Create comprehensive tests in `tests/transaction_rollback_tests.rs`:

```rust
#[test]
fn test_commit_failure_rollback_btree()
#[test]
fn test_commit_failure_rollback_lsm()
#[test]
fn test_commit_failure_rollback_graph()
#[test]
fn test_commit_failure_rollback_vector()
#[test]
fn test_commit_failure_rollback_geospatial()
#[test]
fn test_commit_failure_rollback_fulltext()
#[test]
fn test_partial_commit_recovery()
```

### 5. Performance Considerations

- Undo log collection adds overhead to every commit
- Consider making it optional via a configuration flag
- Benchmark impact on commit latency

### 6. Documentation Updates

- Update `TRANSACTION_SUPPORT.md` with two-phase commit details
- Add recovery scenarios to documentation
- Document limitations (bloom filters, time series, append log)

## Estimated Effort

- Modify commit method: 4-6 hours
- Implement undo execution: 2-3 hours  
- Testing: 3-4 hours
- Documentation: 1-2 hours

**Total: 10-15 hours of focused development**

## Alternative Approach: Compensating Transactions

Instead of collecting undo information upfront, we could:
1. Apply all changes
2. If failure occurs, write compensating operations to WAL
3. Apply compensating operations to restore state

This is simpler but less robust (compensating operations could also fail).