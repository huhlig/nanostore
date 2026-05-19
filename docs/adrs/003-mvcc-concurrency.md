# ADR-003: MVCC Concurrency Control

**Status**: Accepted  
**Date**: 2026-05-10  
**Deciders**: Hans W. Uhlig, Development Team  
**Technical Story**: Transaction and concurrency model

## Context

Nanostore needs a concurrency control mechanism that allows:
- Multiple concurrent readers without blocking
- Writers that don't block readers
- Consistent snapshot views for transactions
- ACID transaction guarantees
- Good performance under concurrent load

Traditional locking approaches (2PL) can cause significant contention and reduce throughput. We need a mechanism that maximizes concurrency while maintaining correctness.

## Decision

We will implement **Multi-Version Concurrency Control (MVCC)** with **Snapshot Isolation**.

**Key Components**:
1. **Version Chains**: Each key maintains a chain of versions
2. **LSN-based Snapshots**: Transactions read from a consistent snapshot
3. **Non-blocking Reads**: Readers never block writers or other readers
4. **Write-Write Conflict Detection**: Detect and abort conflicting writes
5. **Garbage Collection**: Remove old versions when no longer needed

## Consequences

### Positive

- **High Concurrency**: Readers never block writers or other readers
- **Consistent Snapshots**: Transactions see consistent point-in-time view
- **No Deadlocks**: No read-write deadlocks (only write-write conflicts)
- **Time-Travel Queries**: Can read historical data
- **Predictable Performance**: No lock contention for reads

### Negative

- **Storage Overhead**: Multiple versions consume more space
- **Garbage Collection**: Need to clean up old versions
- **Write Amplification**: Each write creates a new version
- **Complexity**: More complex than simple locking
- **Write-Write Conflicts**: Must detect and handle conflicts

### Mitigations

1. **Garbage Collection**: Automatic cleanup of old versions
2. **Compaction**: LSM compaction removes old versions
3. **Conflict Detection**: Fast conflict detection using bloom filters
4. **Version Limits**: Configurable retention policy

## Implementation Details

### Version Chain Structure

```rust
struct VersionChain {
    versions: Vec<Version>,
}

struct Version {
    lsn: LogSequenceNumber,      // When created
    txn_id: TransactionId,        // Who created it
    value: Option<Vec<u8>>,       // None = deleted
}
```

**Storage**:
- B-Tree: Versions stored inline in leaf nodes
- LSM: Versions stored in SSTables, merged during compaction

### Snapshot Isolation

**Transaction Begin**:
```rust
fn begin_transaction() -> Transaction {
    let snapshot_lsn = current_lsn();
    Transaction {
        id: next_txn_id(),
        snapshot_lsn,
        read_set: HashSet::new(),
        write_set: HashMap::new(),
    }
}
```

**Read Operation**:
```rust
fn get(&self, key: &[u8], snapshot_lsn: LSN) -> Option<Value> {
    let chain = self.get_version_chain(key)?;
    
    // Find first version visible at snapshot
    for version in chain.versions {
        if version.lsn <= snapshot_lsn {
            return version.value.clone();
        }
    }
    
    None  // Key didn't exist at snapshot
}
```

**Write Operation**:
```rust
fn put(&mut self, key: &[u8], value: &[u8]) {
    let version = Version {
        lsn: self.next_lsn(),
        txn_id: self.id,
        value: Some(value.to_vec()),
    };
    
    self.write_set.insert(key.to_vec(), version);
}
```

**Commit**:
```rust
fn commit(&mut self) -> Result<()> {
    // 1. Check for write-write conflicts
    for key in self.write_set.keys() {
        if self.has_conflict(key)? {
            return Err(TransactionError::WriteConflict);
        }
    }
    
    // 2. Write to WAL
    self.wal.write_commit(self.id)?;
    
    // 3. Apply writes (create new versions)
    for (key, version) in self.write_set {
        self.table.add_version(key, version)?;
    }
    
    Ok(())
}
```

### Conflict Detection

**Write-Write Conflicts**:
```rust
fn has_conflict(&self, key: &[u8]) -> Result<bool> {
    let chain = self.get_version_chain(key)?;
    
    // Check if any version was created after our snapshot
    for version in chain.versions {
        if version.lsn > self.snapshot_lsn {
            return Ok(true);  // Conflict!
        }
    }
    
    Ok(false)
}
```

**Conflict Resolution**:
- **First-Writer-Wins**: First transaction to commit wins
- **Abort Loser**: Later transaction aborts with conflict error
- **Retry**: Application can retry the transaction

### Garbage Collection

**When to Collect**:
- Version older than oldest active snapshot
- Configurable minimum retention period
- During compaction (LSM)
- Background GC thread (B-Tree)

**GC Algorithm**:
```rust
fn garbage_collect(&mut self) {
    let min_snapshot_lsn = self.get_min_active_snapshot_lsn();
    
    for chain in self.all_version_chains() {
        // Keep only versions visible to active snapshots
        chain.versions.retain(|v| {
            v.lsn >= min_snapshot_lsn || 
            v.lsn >= (current_lsn() - retention_period)
        });
    }
}
```

## Isolation Level: Snapshot Isolation

**Properties**:
- ✅ **Read Committed**: Always read committed data
- ✅ **Repeatable Read**: Same read returns same result
- ✅ **No Phantom Reads**: Range queries are consistent
- ⚠️ **Write Skew**: Possible (not serializable)

**Example of Write Skew**:
```
T1: Read X=100, Y=100
T2: Read X=100, Y=100
T1: Write X=50  (constraint: X+Y >= 100)
T2: Write Y=50  (constraint: X+Y >= 100)
Both commit → X+Y = 100 (OK individually, violates constraint together)
```

**Mitigation**: Application-level constraint checking or serializable isolation.

## Performance Characteristics

### Read Performance

- **Best Case**: O(1) - version at head of chain
- **Average Case**: O(k) where k = versions per key (typically 1-3)
- **Worst Case**: O(n) - traverse entire chain

**Optimization**: Keep chains short via aggressive GC.

### Write Performance

- **Best Case**: O(1) - append to chain
- **Average Case**: O(1) - no conflicts
- **Worst Case**: O(n) - conflict detection

**Optimization**: Bloom filters for fast conflict detection.

### Space Overhead

- **Per Version**: ~40 bytes (LSN + TxnID + value pointer)
- **Typical**: 1-3 versions per key
- **Worst Case**: One version per transaction (before GC)

**Optimization**: Aggressive GC and compaction.

## Alternatives Considered

### Alternative 1: Two-Phase Locking (2PL)

**Approach**: Acquire locks before accessing data, release at commit.

**Pros**:
- Simple to implement
- Serializable isolation
- No version overhead

**Cons**:
- Readers block writers
- Writers block readers
- Deadlock risk
- Poor concurrency

**Rejected because**: Unacceptable concurrency limitations.

### Alternative 2: Optimistic Concurrency Control (OCC)

**Approach**: Read without locks, validate at commit, abort if conflicts.

**Pros**:
- No locking during execution
- Good for low-contention workloads

**Cons**:
- High abort rate under contention
- Wasted work on aborts
- Complex validation phase

**Rejected because**: MVCC provides better performance under contention.

### Alternative 3: Timestamp Ordering

**Approach**: Assign timestamps, enforce timestamp order.

**Pros**:
- No locking
- Deadlock-free

**Cons**:
- Cascading aborts
- Timestamp management overhead
- Poor performance under contention

**Rejected because**: MVCC is more practical and performant.

## Monitoring and Metrics

Track these metrics:
- Average version chain length
- GC frequency and duration
- Conflict rate (aborts per commit)
- Active snapshot count
- Storage overhead (versions vs data)

## Testing Strategy

1. **Concurrent Reads**: Multiple readers, no conflicts
2. **Read-Write**: Readers see consistent snapshot
3. **Write-Write Conflicts**: Detect and abort correctly
4. **Garbage Collection**: Old versions removed
5. **Long Transactions**: Handle long-running snapshots
6. **Stress Tests**: High concurrency, many versions

## References

- [PostgreSQL MVCC](https://www.postgresql.org/docs/current/mvcc.html)
- [MySQL InnoDB MVCC](https://dev.mysql.com/doc/refman/8.0/en/innodb-multi-versioning.html)
- [Snapshot Isolation Paper](https://www.microsoft.com/en-us/research/publication/a-critique-of-ansi-sql-isolation-levels/)
- [Transaction Layer Implementation](../../src/txn/)

## Related ADRs

- [ADR-005: Write-Ahead Logging](./005-write-ahead-logging.md)
- [ADR-006: Sharded Concurrency](./006-sharded-concurrency.md)
- [ADR-004: Multiple Storage Engines](./004-multiple-storage-engines.md)

---

**Last Updated**: 2026-05-10