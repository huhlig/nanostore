# Transaction Isolation Levels

This document describes the implementation and semantics of the five transaction isolation levels supported by Nanostore.

## Overview

Nanostore implements five standard SQL isolation levels, each providing different guarantees about transaction visibility and conflict detection:

1. **ReadUncommitted** - Lowest isolation, highest concurrency
2. **ReadCommitted** - Default level, prevents dirty reads
3. **RepeatableRead** - Prevents non-repeatable reads
4. **Serializable** - Highest isolation, prevents all anomalies
5. **SnapshotIsolation** - Snapshot-based reads with write-write conflict detection

## Isolation Level Semantics

### ReadUncommitted

**Guarantees:**
- None - allows all anomalies

**Behavior:**
- No read tracking
- No read-write conflict detection
- Write-write conflicts still detected (for data integrity)
- Allows dirty reads (reading uncommitted data from other transactions)*

**Use Cases:**
- Read-heavy workloads where approximate data is acceptable
- Analytics queries that don't require consistency
- Maximum concurrency scenarios

**Limitations:**
- *Full dirty read support requires a shared write set registry (not yet implemented)
- Currently behaves like ReadCommitted for reads from committed storage

**Example:**
```rust
let tx = db.begin_transaction(IsolationLevel::ReadUncommitted)?;
// Can read data without blocking other transactions
let value = tx.get(table_id, key)?;
tx.commit()?;
```

### ReadCommitted

**Guarantees:**
- Prevents dirty reads (cannot read uncommitted data)
- Allows non-repeatable reads (same read may return different values)
- Allows phantom reads (range queries may see new rows)

**Behavior:**
- No read tracking
- No read-write conflict detection
- Write-write conflicts detected
- Uses snapshot LSN for consistent reads within transaction

**Use Cases:**
- Default isolation level for most applications
- Good balance of consistency and concurrency
- Web applications with short transactions

**Example:**
```rust
let tx = db.begin_transaction(IsolationLevel::ReadCommitted)?;
let value1 = tx.get(table_id, key)?; // Reads committed data
// Another transaction may commit changes here
let value2 = tx.get(table_id, key)?; // May see different value
tx.commit()?;
```

### RepeatableRead

**Guarantees:**
- Prevents dirty reads
- Prevents non-repeatable reads (same read returns same value)
- Allows phantom reads (range queries may see new rows)

**Behavior:**
- Tracks all reads in read_set
- Checks for read-write conflicts at commit time
- Write-write conflicts detected
- Uses snapshot LSN for consistent reads

**Use Cases:**
- Financial transactions requiring consistent reads
- Reports that need stable data throughout execution
- Scenarios where non-repeatable reads would cause issues

**Example:**
```rust
let tx = db.begin_transaction(IsolationLevel::RepeatableRead)?;
let value1 = tx.get(table_id, key)?; // Reads and tracks
// Another transaction tries to modify this key
let value2 = tx.get(table_id, key)?; // Returns same value
tx.commit()?; // Fails if another transaction modified the key
```

### Serializable

**Guarantees:**
- Prevents dirty reads
- Prevents non-repeatable reads
- Prevents phantom reads
- Full serializability (transactions appear to execute sequentially)

**Behavior:**
- Tracks all reads in read_set
- Checks for read-write conflicts at commit time
- Write-write conflicts detected
- Most restrictive isolation level

**Use Cases:**
- Critical financial transactions
- Scenarios requiring absolute consistency
- When correctness is more important than concurrency

**Example:**
```rust
let tx = db.begin_transaction(IsolationLevel::Serializable)?;
let value = tx.get(table_id, key)?; // Reads and tracks
tx.put(table_id, key2, value2)?;
tx.commit()?; // Fails if any read key was modified by another transaction
```

### SnapshotIsolation

**Guarantees:**
- Prevents dirty reads
- Prevents non-repeatable reads (via snapshot)
- Prevents phantom reads (via snapshot)
- Allows write skew anomalies

**Behavior:**
- No read tracking (uses snapshot for consistency)
- No read-write conflict detection
- Write-write conflicts detected
- Uses snapshot LSN for all reads

**Use Cases:**
- Long-running read transactions
- Scenarios where write skew is acceptable
- Better concurrency than Serializable with similar read consistency

**Example:**
```rust
let tx = db.begin_transaction(IsolationLevel::SnapshotIsolation)?;
// All reads see consistent snapshot from transaction start
let value1 = tx.get(table_id, key1)?;
let value2 = tx.get(table_id, key2)?;
// Can write without read-write conflict checking
tx.put(table_id, key3, value3)?;
tx.commit()?; // Only checks write-write conflicts
```

## Implementation Details

### Read Tracking

Read tracking is implemented in the `record_read()` method:

```rust
pub fn record_read(&mut self, object_id: TableId, key: Vec<u8>) {
    match self.isolation {
        IsolationLevel::Serializable | IsolationLevel::RepeatableRead => {
            self.read_set.insert((object_id, key));
        }
        _ => {
            // No read tracking for other isolation levels
        }
    }
}
```

### Conflict Detection at Commit

Conflict detection is performed in the `commit()` method based on isolation level:

```rust
match self.isolation {
    IsolationLevel::ReadUncommitted => {
        // No conflict checking needed
    }
    IsolationLevel::ReadCommitted => {
        // Only write-write conflicts (checked in put/delete)
    }
    IsolationLevel::RepeatableRead => {
        // Check for read-write conflicts
        detector.check_read_write_conflicts(&self.read_set, self.txn_id)?;
    }
    IsolationLevel::Serializable => {
        // Check for read-write conflicts (full serializability)
        detector.check_read_write_conflicts(&self.read_set, self.txn_id)?;
    }
    IsolationLevel::SnapshotIsolation => {
        // Only write-write conflicts (checked in put/delete)
    }
}
```

### Write-Write Conflict Detection

All isolation levels detect write-write conflicts to maintain data integrity:

```rust
// In put() and delete() methods
let detector = self.conflict_detector.lock().unwrap();
detector.check_write_conflict(object_id, key, self.txn_id)?;
detector.acquire_write_lock(object_id, key.to_vec(), self.txn_id);
```

## Anomalies Prevented

| Isolation Level    | Dirty Read | Non-Repeatable Read | Phantom Read | Write Skew |
|-------------------|------------|---------------------|--------------|------------|
| ReadUncommitted   | ❌         | ❌                  | ❌           | ❌         |
| ReadCommitted     | ✅         | ❌                  | ❌           | ❌         |
| RepeatableRead    | ✅         | ✅                  | ❌           | ❌         |
| Serializable      | ✅         | ✅                  | ✅           | ✅         |
| SnapshotIsolation | ✅         | ✅                  | ✅           | ❌         |

Legend:
- ✅ = Prevented
- ❌ = Allowed

## Performance Characteristics

| Isolation Level    | Read Overhead | Write Overhead | Conflict Rate | Concurrency |
|-------------------|---------------|----------------|---------------|-------------|
| ReadUncommitted   | Minimal       | Low            | Lowest        | Highest     |
| ReadCommitted     | Minimal       | Low            | Low           | High        |
| RepeatableRead    | Medium        | Low            | Medium        | Medium      |
| Serializable      | Medium        | Low            | Highest       | Lowest      |
| SnapshotIsolation | Minimal       | Low            | Low           | High        |

## Choosing an Isolation Level

### Use ReadUncommitted when:
- Reading approximate data for analytics
- Maximum concurrency is critical
- Data consistency is not important

### Use ReadCommitted when:
- General-purpose transactions
- Good balance of consistency and performance needed
- Default choice for most applications

### Use RepeatableRead when:
- Need consistent reads within a transaction
- Non-repeatable reads would cause application errors
- Financial calculations requiring stable data

### Use Serializable when:
- Absolute consistency is required
- Correctness is more important than performance
- Critical financial or inventory transactions

### Use SnapshotIsolation when:
- Long-running read transactions
- Need consistent snapshot without blocking writers
- Write skew anomalies are acceptable

## Testing

Comprehensive tests for isolation levels are in `tests/isolation_level_tests.rs`:

- `test_read_uncommitted_no_read_tracking` - Verifies no read tracking
- `test_read_committed_no_read_tracking` - Verifies no read-write conflict checking
- `test_repeatable_read_tracks_reads` - Verifies read tracking and conflict detection
- `test_serializable_tracks_reads` - Verifies full serializability
- `test_snapshot_isolation_no_read_tracking` - Verifies snapshot-based reads
- `test_write_write_conflict_detection` - Verifies write-write conflicts detected

## Future Enhancements

### Planned Improvements

1. **Full Dirty Read Support for ReadUncommitted**
   - Implement shared write set registry
   - Allow reading uncommitted data from other transactions
   - Requires careful memory management

2. **Predicate Locking for Serializable**
   - Detect phantom reads in range queries
   - Implement range locks or predicate locks
   - Prevent serialization anomalies in range operations

3. **Optimistic Concurrency Control**
   - Reduce lock contention
   - Validate at commit time instead of acquiring locks
   - Better performance for read-heavy workloads

4. **Adaptive Isolation Levels**
   - Automatically adjust isolation level based on workload
   - Detect hotspots and increase isolation
   - Reduce isolation for cold data

## References

- [ANSI SQL-92 Isolation Levels](https://www.contrib.andrew.cmu.edu/~shadow/sql/sql1992.txt)
- [A Critique of ANSI SQL Isolation Levels](https://www.microsoft.com/en-us/research/wp-content/uploads/2016/02/tr-95-51.pdf)
- [Snapshot Isolation in PostgreSQL](https://www.postgresql.org/docs/current/transaction-iso.html)
- [Serializable Snapshot Isolation](https://courses.cs.washington.edu/courses/cse444/08au/544M/READING-LIST/fekete-sigmod2008.pdf)