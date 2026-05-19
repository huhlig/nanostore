# Transaction Support Implementation

**Date**: 2026-05-11  
**Status**: Completed  
**Issue**: Nanostore-g3n

---

## Overview

This document describes the implementation of ACID transaction support in Nanostore with full WAL (Write-Ahead Log) integration for durability and crash recovery.

## Architecture

### Key Components

1. **Database Layer** (`src/kvdb.rs`)
   - Owns the WAL writer and table storage
   - Manages transaction lifecycle (begin/commit/rollback)
   - Coordinates between transactions and storage engines

2. **Transaction Layer** (`src/txn/transaction.rs`)
   - Implements ACID transaction semantics
   - Maintains write sets for uncommitted changes
   - Integrates with WAL for durability
   - Supports conflict detection and isolation

3. **WAL Integration** (`src/wal/`)
   - Records BEGIN, WRITE, COMMIT, and ROLLBACK operations
   - Provides durability guarantees
   - Enables crash recovery

### Design Principles

#### 1. Data Structures as Data (Tables)

Following ADR-011, both tables and indexes are treated uniformly at the transaction layer using `ObjectId`. The transaction layer does NOT automatically maintain indexes - that responsibility belongs to the API consumer (Database layer or query engine).

**Benefits:**
- Simple transaction layer focused on ACID properties
- Flexible index maintenance strategies (synchronous, deferred, async)
- Supports any index type without transaction layer changes
- Makes index updates explicit and visible in the transaction's write set

#### 2. 1:1 Transaction Mapping

Each Nanostore transaction maps 1:1 to a higher-level transaction. This provides:
- Predictable transaction boundaries
- Clear durability guarantees
- Simplified recovery logic

#### 3. WAL-First Durability

All transaction operations are written to the WAL before being applied to storage:
- BEGIN record when transaction starts
- WRITE records for each put/delete operation
- COMMIT record when transaction commits successfully
- ROLLBACK record when transaction aborts

## Implementation Details

### Database Structure

```rust
pub struct Database<FS: FileSystem> {
    // Transaction management
    conflict_detector: Arc<Mutex<ConflictDetector>>,
    next_txn_id: Arc<Mutex<u64>>,
    current_lsn: Arc<RwLock<LogSequenceNumber>>,
    
    // Catalog management
    table_catalog: Arc<RwLock<HashMap<String, TableInfo>>>,
    
    // Storage layer
    wal: Arc<WalWriter<FS>>,
    table_storage: Arc<RwLock<HashMap<ObjectId, HashMap<Vec<u8>, Vec<u8>>>>>,
}
```

### Transaction Structure

```rust
pub struct Transaction<FS: FileSystem> {
    // Core transaction identity
    txn_id: TransactionId,
    snapshot_lsn: LogSequenceNumber,
    isolation: IsolationLevel,
    state: TransactionState,
    
    // Tracking sets
    read_set: HashSet<(ObjectId, Vec<u8>)>,  // For Serializable isolation
    write_set: HashMap<(ObjectId, Vec<u8>), Option<Vec<u8>>>,
    
    // Shared resources
    conflict_detector: Arc<Mutex<ConflictDetector>>,
    wal: Arc<WalWriter<FS>>,
    table_storage: Arc<RwLock<HashMap<ObjectId, HashMap<Vec<u8>, Vec<u8>>>>>,
    current_lsn: Arc<RwLock<LogSequenceNumber>>,
}
```

### Transaction Lifecycle

#### 1. Begin Transaction

```rust
pub fn begin_write(&self, durability: Durability) -> Result<Transaction<FS>, DatabaseError> {
    let txn_id = self.allocate_txn_id();
    let snapshot_lsn = *self.current_lsn.read().unwrap();
    
    // Write BEGIN record to WAL
    self.wal.write_begin(txn_id)?;
    
    Ok(Transaction::new(
        txn_id,
        snapshot_lsn,
        IsolationLevel::ReadCommitted,
        Arc::clone(&self.conflict_detector),
        Arc::clone(&self.wal),
        Arc::clone(&self.table_storage),
        Arc::clone(&self.current_lsn),
    ))
}
```

#### 2. Write Operations

```rust
pub fn put(&mut self, object: ObjectId, key: &[u8], value: &[u8]) -> TransactionResult<()> {
    // Check for write-write conflicts
    let mut detector = self.conflict_detector.lock().unwrap();
    detector.check_write_conflict(object, key, self.txn_id)?;
    detector.acquire_write_lock(object, key.to_vec(), self.txn_id);
    
    // Write to WAL
    self.wal.write_operation(
        self.txn_id,
        object,
        WriteOpType::Put,
        key.to_vec(),
        value.to_vec(),
    )?;
    
    // Record in write set
    self.record_write(object, key.to_vec(), value.to_vec());
    Ok(())
}
```

#### 3. Commit Transaction

```rust
pub fn commit(mut self) -> TransactionResult<CommitInfo> {
    // Check for conflicts (Serializable isolation)
    if self.isolation == IsolationLevel::Serializable {
        let detector = self.conflict_detector.lock().unwrap();
        detector.check_read_write_conflicts(&self.read_set, self.txn_id)?;
    }
    
    // Write COMMIT record to WAL
    let commit_lsn = self.wal.write_commit(self.txn_id)?;
    
    // Apply write set to storage
    let mut storage = self.table_storage.write().unwrap();
    for ((object_id, key), value_opt) in &self.write_set {
        let table = storage.entry(*object_id).or_insert_with(HashMap::new);
        match value_opt {
            Some(value) => table.insert(key.clone(), value.clone()),
            None => table.remove(key),
        };
    }
    
    // Update current LSN
    *self.current_lsn.write().unwrap() = commit_lsn;
    
    // Release locks
    self.conflict_detector.lock().unwrap().release_locks(self.txn_id);
    
    Ok(CommitInfo {
        tx_id: self.txn_id,
        commit_lsn,
        durable_lsn: Some(commit_lsn),
    })
}
```

#### 4. Rollback Transaction

```rust
pub fn rollback(mut self) -> TransactionResult<()> {
    // Write ROLLBACK record to WAL
    self.wal.write_rollback(self.txn_id)?;
    
    // Release locks
    self.conflict_detector.lock().unwrap().release_locks(self.txn_id);
    
    // Write set is automatically dropped
    Ok(())
}
```

### Read Operations

Transactions read from two sources:
1. **Write set** (uncommitted changes in current transaction)
2. **Table storage** (committed data from other transactions)

```rust
pub fn get(&self, object: ObjectId, key: &[u8]) -> TransactionResult<Option<ValueBuf>> {
    // Check write set first
    let write_key = (object, key.to_vec());
    if let Some(value_opt) = self.write_set.get(&write_key) {
        return Ok(value_opt.as_ref().map(|v| ValueBuf(v.clone())));
    }
    
    // Read from committed storage
    let storage = self.table_storage.read().unwrap();
    if let Some(table) = storage.get(&object) {
        if let Some(value) = table.get(key) {
            return Ok(Some(ValueBuf(value.clone())));
        }
    }
    
    Ok(None)
}
```

## Multi-Table Transactions

Transactions naturally support multiple tables through the unified `ObjectId` abstraction:

```rust
let mut txn = db.begin_write(Durability::WalOnly)?;

let users_table = ObjectId::from(1);
let posts_table = ObjectId::from(2);
let comments_table = ObjectId::from(3);

txn.put(users_table, b"user:1", b"Alice")?;
txn.put(posts_table, b"post:1", b"Hello World")?;
txn.put(comments_table, b"comment:1", b"Nice post!")?;

txn.commit()?;  // All changes committed atomically
```

## Isolation Levels

Currently implemented:
- **ReadCommitted**: Default isolation level
  - Reads see committed data
  - No dirty reads
  - Write-write conflicts detected

Future support (via MVCC):
- **Serializable**: Full serializability
  - Read-write conflict detection
  - Prevents all anomalies

## Durability Guarantees

### WAL-Only Durability

```rust
let txn = db.begin_write(Durability::WalOnly)?;
// ... operations ...
let commit_info = txn.commit()?;
assert!(commit_info.durable_lsn.is_some());  // Changes are durable
```

The WAL is synced to disk on commit, providing durability guarantees:
- Committed transactions survive crashes
- Uncommitted transactions are rolled back on recovery
- WAL replay restores database state

## Testing

Comprehensive test suite in `tests/transaction_wal_integration_tests.rs`:

- ✅ Basic commit/rollback
- ✅ Read uncommitted changes within transaction
- ✅ Delete operations
- ✅ Update operations
- ✅ Multi-table transactions
- ✅ Transaction isolation
- ✅ Multiple operations on same key
- ✅ Put-delete-put sequences
- ✅ Empty transactions
- ✅ Sequential transactions

All 13 tests passing.

## Performance Characteristics

### Write Path
1. Conflict detection: O(1) hash lookup
2. WAL write: Sequential write to log
3. Write set update: O(1) hash insert
4. Commit: O(n) where n = write set size

### Read Path
1. Write set lookup: O(1) hash lookup
2. Storage lookup: O(1) hash lookup
3. Total: O(1) for point lookups

### Memory Usage
- Write set: O(n) where n = number of writes
- Read set: O(m) where m = number of reads (Serializable only)
- Locks: O(k) where k = number of locked keys

## Future Enhancements

1. **MVCC Support** (Nanostore-3ya)
   - Multi-version concurrency control
   - Non-blocking reads
   - Snapshot isolation
   - Serializable isolation

2. **Range Operations**
   - Range scans
   - Range deletes
   - Prefix scans

3. **Savepoints**
   - Partial rollback
   - Nested transactions

4. **Distributed Transactions**
   - Two-phase commit
   - Distributed deadlock detection

## Related Documentation

- [ADR-011: Indexes as Specialty Tables](adrs/011-indexes-as-specialty-tables.md)
- [ADR-003: MVCC Concurrency](adrs/003-mvcc-concurrency.md)
- [WAL Implementation](archive/WAL_IMPLEMENTATION.md)
- [Architecture Overview](ARCHITECTURE.md)

## References

- Issue: Nanostore-g3n "Phase 4: Transaction Support"
- Implementation: `src/kvdb.rs`, `src/txn/transaction.rs`
- Tests: `tests/transaction_wal_integration_tests.rs`