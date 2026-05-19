# WAL Group Commit Implementation

## Overview

Group commit is a performance optimization technique that batches multiple transaction commits into a single fsync operation. This significantly improves throughput under high concurrency by amortizing the cost of expensive disk synchronization across multiple transactions.

## Implementation Status

✅ **COMPLETE** - Group commit has been fully implemented and tested.

## Architecture

### Components

1. **GroupCommitCoordinator** - Background thread that manages commit batching
2. **CommitQueue** - Queue of pending commit requests
3. **GroupCommitConfig** - Configuration for batching behavior
4. **GroupCommitMetrics** - Performance tracking and monitoring

### How It Works

```
┌─────────────┐
│ Transaction │
│   Thread    │
└──────┬──────┘
       │ write_commit()
       ▼
┌─────────────────┐
│  Commit Queue   │
│  (pending)      │
└──────┬──────────┘
       │
       ▼
┌─────────────────┐
│  Coordinator    │◄─── Background Thread
│  (batching)     │
└──────┬──────────┘
       │ Batch Ready
       ▼
┌─────────────────┐
│   fsync()       │◄─── Single fsync for entire batch
└──────┬──────────┘
       │
       ▼
┌─────────────────┐
│ Notify All      │
│ Waiting Threads │
└─────────────────┘
```

## Configuration

### Presets

Three configuration presets are provided:

```rust
// High throughput (default)
GroupCommitConfig::high_throughput()
// - max_batch_size: 100
// - max_wait_micros: 1000 (1ms)
// - min_batch_size: 10

// Balanced
GroupCommitConfig::balanced()
// - max_batch_size: 50
// - max_wait_micros: 500 (0.5ms)
// - min_batch_size: 5

// Low latency (disables group commit)
GroupCommitConfig::low_latency()
// - enabled: false
```

### Custom Configuration

```rust
use Nanostore::wal::{GroupCommitConfig, WalWriterConfig};

let mut config = WalWriterConfig::default();
config.group_commit = GroupCommitConfig {
    enabled: true,
    max_batch_size: 75,
    max_wait_micros: 750,
    min_batch_size: 8,
};
```

### Configuration Parameters

- **enabled**: Enable/disable group commit optimization
- **max_batch_size**: Maximum commits to batch together (triggers immediate flush)
- **max_wait_micros**: Maximum time to wait for batching (microseconds)
- **min_batch_size**: Minimum batch size to trigger early flush

## Usage

### Basic Usage

```rust
use Nanostore::vfs::LocalFileSystem;
use Nanostore::wal::{GroupCommitConfig, WalWriter, WalWriterConfig, WriteOpType};

let fs = LocalFileSystem::new("./data");
let mut config = WalWriterConfig::default();
config.group_commit = GroupCommitConfig::high_throughput();

let writer = WalWriter::create(&fs, "database.wal", config)?;

// Commits are automatically batched
writer.write_begin(1)?;
writer.write_operation(1, "users".to_string(), WriteOpType::Put, 
    b"user:1".to_vec(), b"Alice".to_vec())?;
writer.write_commit(1)?; // Batched with other concurrent commits
```

### Concurrent Usage

```rust
use std::sync::Arc;
use std::thread;

let writer = Arc::new(writer);

let mut handles = vec![];
for i in 0..10 {
    let writer_clone = writer.clone();
    let handle = thread::spawn(move || {
        writer_clone.write_begin(i)?;
        writer_clone.write_operation(i, "data".to_string(), 
            WriteOpType::Put, format!("key{}", i).into_bytes(), 
            b"value".to_vec())?;
        writer_clone.write_commit(i)?; // All batched together
        Ok::<(), WalError>(())
    });
    handles.push(handle);
}

for handle in handles {
    handle.join().unwrap()?;
}
```

### Checking Metrics

```rust
if let Some(metrics) = writer.group_commit_metrics() {
    println!("Total commits: {}", 
        metrics.total_commits.load(Ordering::Relaxed));
    println!("Total fsyncs: {}", 
        metrics.total_fsyncs.load(Ordering::Relaxed));
    println!("Average batch size: {:.2}", metrics.avg_batch_size());
    println!("Fsync reduction: {:.2}x", metrics.fsync_reduction_ratio());
}
```

## Performance

### Test Results

From integration tests (`tests/wal_group_commit_tests.rs`):

**Concurrent Transactions (50 threads, 1 txn each):**
- Total commits: 50
- Total fsyncs: 5
- **10x fsync reduction**

**Stress Test (20 threads, 50 txns each):**
- Total commits: 1,000
- Total fsyncs: 53
- **18.87x fsync reduction**

### Expected Improvements

Based on typical database implementations:

- **Throughput**: 5-10x improvement under high concurrency
- **Latency**: Slight increase (1-2ms) due to batching delay
- **Fsync Reduction**: 10-100x fewer fsync calls
- **CPU Usage**: Minimal overhead from coordinator thread

### When to Use

**Use Group Commit When:**
- High transaction throughput is required
- Multiple concurrent transactions
- Disk I/O is a bottleneck
- Can tolerate 1-2ms additional latency

**Disable Group Commit When:**
- Ultra-low latency is critical
- Single-threaded workload
- Very low transaction rate
- Testing/debugging commit behavior

## Batching Strategy

The coordinator uses a hybrid batching approach:

1. **Batch Full**: Flush immediately when `max_batch_size` is reached
2. **Timeout + Min Batch**: Flush when `max_wait_micros` elapsed AND `min_batch_size` reached
3. **Timeout Only**: Flush when `max_wait_micros` elapsed (even if batch is small)

This ensures:
- High throughput under load (large batches)
- Bounded latency (timeout prevents indefinite waiting)
- Efficient batching (minimum batch size prevents tiny batches)

## Metrics

### Available Metrics

```rust
pub struct GroupCommitMetrics {
    pub total_commits: AtomicU64,      // Total commits processed
    pub total_fsyncs: AtomicU64,       // Total fsync operations
    pub total_batches: AtomicU64,      // Total batches processed
    pub max_batch_size: AtomicU64,     // Largest batch seen
    pub total_wait_time_us: AtomicU64, // Total wait time
}
```

### Derived Metrics

- **avg_batch_size()**: Average commits per batch
- **avg_wait_time_us()**: Average wait time per commit
- **fsync_reduction_ratio()**: Commits per fsync (higher is better)

## Error Handling

### Coordinator Failure

If the coordinator thread panics or fails:
- Pending commits receive error notification
- System falls back to direct fsync
- Error is logged for investigation

### Fsync Failure

If fsync fails:
- All pending commits in the batch receive the error
- Queue is cleared
- Next batch starts fresh
- Consistency is maintained

### Queue Full

Currently, the queue is unbounded. Future versions may add:
- Configurable queue capacity
- Backpressure mechanisms
- Queue full policies (block, error, drop)

## Thread Safety

Group commit is fully thread-safe:
- Multiple threads can commit concurrently
- Coordinator runs in dedicated background thread
- All synchronization uses lock-free or fine-grained locking
- No deadlocks or race conditions

## Compatibility

### Backward Compatibility

- Group commit is opt-in via configuration
- When group commit is disabled, default behavior unchanged (sync_on_write=true)
- When group commit is enabled, sync_on_write is ignored (coordinator handles syncing)
- No changes to WAL file format
- No changes to recovery logic
- Existing code works without modification

### Forward Compatibility

- Metrics can be extended without breaking changes
- Configuration can add new fields
- Batching strategy can be improved
- Alternative coordinators can be implemented

## Testing

### Unit Tests

Located in `src/wal/group_commit.rs`:
- Configuration presets
- Metrics tracking
- Queue operations
- Batching logic

### Integration Tests

Located in `tests/wal_group_commit_tests.rs`:
- Single-threaded commits
- Concurrent commits
- Stress testing
- Configuration presets
- Metrics validation
- Rollback handling

### Benchmarks

Located in `benches/wal_benchmarks.rs`:
- Single-threaded throughput
- Concurrent throughput (2, 4, 8, 16 threads)
- Configuration comparison
- With/without group commit comparison

## Troubleshooting

### Low Fsync Reduction

If fsync reduction is lower than expected:
- Increase `max_batch_size`
- Increase `max_wait_micros`
- Check if workload is truly concurrent
- Verify group commit is enabled

### High Latency

If commit latency is too high:
- Decrease `max_wait_micros`
- Decrease `min_batch_size`
- Consider using `balanced` or `low_latency` preset
- Check if coordinator thread is starved

### Coordinator Not Running

If metrics show no batching:
- Verify `enabled: true` in configuration
- Check for coordinator thread panics in logs
- Ensure multiple concurrent commits
- Verify writer is shared across threads

## Future Enhancements

Potential improvements for future versions:

1. **Adaptive Batching**: Dynamically adjust batch size based on load
2. **Priority Commits**: Fast-path for high-priority transactions
3. **Multiple Coordinators**: Parallel batching for extreme throughput
4. **Queue Capacity**: Configurable queue limits with backpressure
5. **Metrics Export**: Prometheus/OpenTelemetry integration
6. **Commit Callbacks**: Async notification of commit completion

## References

- [PostgreSQL Group Commit](https://www.postgresql.org/docs/current/wal-async-commit.html)
- [MySQL InnoDB Group Commit](https://dev.mysql.com/doc/refman/8.0/en/innodb-parameters.html#sysvar_innodb_flush_log_at_trx_commit)
- "Transaction Processing: Concepts and Techniques" by Gray & Reuter
- "The Design and Implementation of Modern Column-Oriented Database Systems"

## See Also

- [WAL Implementation](WAL_IMPLEMENTATION.md)
- [WAL Concurrency Tests](WAL_CONCURRENCY_TESTS.md)
- [Group Commit Design](WAL_GROUP_COMMIT_DESIGN.md)