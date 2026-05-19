# Transaction Layer Structural Improvements

**Issue:** Nanostore-i5l  
**Date:** 2026-05-10  
**Status:** Completed

## Overview

This document summarizes the structural improvements made to the transaction layer to enable actual transaction implementation and support multiple storage engines.

## Changes Implemented

### 1. Transaction Trait for Multiple Storage Engines

**Problem:** Transaction was a concrete struct, making it difficult to support different storage engines with different transaction semantics (in-memory BTree vs LSM with WAL).

**Solution:** Added `TransactionOps` trait that defines the core transaction interface:

```rust
pub trait TransactionOps {
    fn id(&self) -> TransactionId;
    fn isolation_level(&self) -> IsolationLevel;
    fn snapshot_lsn(&self) -> LogSequenceNumber;
    fn get(&self, table: TableId, key: &[u8]) -> TransactionResult<Option<ValueBuf>>;
    fn put(&mut self, table: TableId, key: &[u8], value: &[u8]) -> TransactionResult<()>;
    fn delete(&mut self, table: TableId, key: &[u8]) -> TransactionResult<bool>;
    fn range_delete(&mut self, table: TableId, bounds: ScanBounds) -> TransactionResult<u64>;
    fn commit(self) -> TransactionResult<CommitInfo>;
    fn rollback(self) -> TransactionResult<()>;
}
```

The concrete `Transaction` struct now implements this trait, allowing future storage engines to provide their own implementations.

### 2. ConflictDetector Integration

**Problem:** Transaction had write_set/read_set but ConflictDetector was not integrated. The intended flow (check → acquire → write → release) existed only as comments.

**Solution:** 
- Added `conflict_detector: Arc<Mutex<ConflictDetector>>` field to Transaction
- Updated `put()` and `delete()` methods to check for conflicts and acquire locks before recording writes
- Updated `commit()` to check read-write conflicts for Serializable isolation and release locks
- Updated `rollback()` to release locks

**Flow:**
1. **Write operations:** Check conflict → Acquire lock → Record in write_set
2. **Commit:** Check read-write conflicts (Serializable) → Release locks
3. **Rollback:** Release locks

### 3. Write Set Tombstone Support

**Status:** Already implemented correctly.

The write_set already used `HashMap<(TableId, Vec<u8>), Option<Vec<u8>>>` where `None` represents a tombstone (deleted key).

### 4. Transaction Method Implementations

**Status:** Already implemented correctly.

Methods `id()`, `isolation_level()`, and `snapshot_lsn()` were already properly implemented (not returning `todo!()`).

### 5. Cursor Wrapping TableCursor

**Status:** Already implemented correctly.

The `Cursor` struct in `txn/cursor.rs` already wraps `Box<dyn TableCursor>` and delegates to it, providing a uniform interface for transaction-level operations.

### 6. Database Internal Fields

**Problem:** Database struct had no fields for WAL, pager, catalog, or transaction manager.

**Solution:** Added comprehensive internal fields:

```rust
pub struct Database {
    // Transaction management
    conflict_detector: Arc<Mutex<ConflictDetector>>,
    next_txn_id: Arc<Mutex<u64>>,
    current_lsn: Arc<RwLock<LogSequenceNumber>>,
    
    // Catalog management
    table_catalog: Arc<RwLock<HashMap<String, TableInfo>>>,
    index_catalog: Arc<RwLock<HashMap<IndexId, IndexInfo>>>,
    
    // TODO: Storage layer fields (WAL, Pager, etc.)
}
```

Implemented:
- `new()` - Creates a new database instance
- `allocate_txn_id()` - Allocates unique transaction IDs
- `begin_read()` - Creates read-only transactions
- `begin_write()` - Creates write transactions
- `begin_read_at()` - Creates transactions at specific LSN

### 7. Transactional DDL Operations

**Problem:** DDL operations (create_table, create_index, etc.) were not transactional.

**Solution:** Implemented DDL operations with transactional semantics:

- **create_table()**: Checks for duplicates, allocates table ID, records creation LSN
- **drop_table()**: Removes table from catalog (TODO: also remove dependent indexes)
- **open_table()**: Looks up table by name
- **list_tables()**: Returns all tables in catalog
- **create_index()**: Allocates index ID, records creation LSN, marks as fresh
- **drop_index()**: Removes index from catalog
- **list_indexes()**: Returns indexes for a table

All operations use the current LSN to timestamp changes, simulating transactional visibility.

## Testing

All tests pass:
- **Transaction integration tests:** 22 tests passing
- **Library unit tests:** 227 tests passing

## Architecture Notes

### Transaction Lifecycle

1. **Begin:** Database allocates transaction ID and creates Transaction with shared ConflictDetector
2. **Operations:** Transaction checks conflicts, acquires locks, records changes in write_set
3. **Commit:** Validates state, checks conflicts, releases locks, returns CommitInfo
4. **Rollback:** Releases locks, discards write_set

### Conflict Detection

- **Write-Write:** Detected when acquiring locks in `put()`/`delete()`
- **Read-Write:** Detected at commit time for Serializable isolation
- **Lock Management:** Locks acquired per-key, released at commit/rollback

### Design Philosophy: Tables and Indices as Unified Key-Value Stores

In this architecture, both tables and indices are fundamentally the same: key-value stores identified by a logical ID (TableId). The Transaction layer operates on this unified abstraction:

- **Transaction operations** use `TableId` for both tables and indices
- **Higher-level Database layer** provides semantic distinction between tables and indices
- **Index maintenance** is the responsibility of the Database layer, which:
  - Translates table operations into corresponding index operations
  - Issues multiple Transaction operations (one for table, one per index)
  - Ensures atomicity across all related operations

This design allows:
- Simple, unified transaction implementation
- Flexibility in index implementation strategies
- Clear separation of concerns (Transaction = ACID, Database = semantics)

### Current Design Note: TableId vs IndexId

The Transaction layer currently uses `TableId` for all operations. Since both tables and indices are storage structures that need transactional operations, a design decision is needed on how to handle their identifiers:

- **Current:** Separate `TableId` and `IndexId` types, Transaction only uses `TableId`
- **Future:** May need unified `ObjectId` or conversion mechanism
- **See:** Issue Nanostore-6nx for design options and decision

### Future Work

1. **Object ID System:** Design unified ID system for catalog (Nanostore-6nx)
2. **Index Maintenance:** Implement Database layer logic for automatic index updates (Nanostore-j89)
3. **WAL Integration:** Write commit records to WAL
4. **Table Engine Integration:** Apply write_set to actual storage engines
5. **Deadlock Detection:** Integrate DeadlockDetector for cycle detection
6. **Snapshot Management:** Implement proper snapshot lifecycle
7. **DDL Transactions:** Make DDL operations part of regular transactions

## Files Modified

- `src/txn/transaction.rs` - Added trait, integrated ConflictDetector
- `src/kvdb.rs` - Added internal fields, implemented DDL operations
- `tests/transaction_integration_tests.rs` - Updated test helper

## Impact

These changes enable:
- Multiple storage engine support through trait abstraction
- Proper conflict detection and lock management
- Foundation for full ACID transaction implementation
- Transactional DDL operations with LSN-based visibility

## References

- Issue: Nanostore-i5l
- ADR-003: MVCC Concurrency
- ADR-006: Sharded Concurrency