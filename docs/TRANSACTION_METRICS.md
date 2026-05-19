# Transaction Layer Metrics and Tracing

This document describes the observability instrumentation added to the transaction layer in Nanostore.

## Overview

The transaction layer now includes comprehensive metrics and tracing to monitor transaction performance, conflicts, and failures. This enables production monitoring, debugging, and performance analysis.

## Metrics

All metrics use the `Nanostore.transaction.*` namespace.

### Transaction Lifecycle Metrics

#### `Nanostore.transaction.begin.total` (Counter)
- **Description**: Total number of transactions started
- **Type**: Counter
- **When recorded**: When `Transaction::new()` is called
- **Use case**: Track transaction creation rate

#### `Nanostore.transaction.commit.total` (Counter)
- **Description**: Total number of successful transaction commits
- **Type**: Counter
- **When recorded**: When `Transaction::commit()` completes successfully
- **Use case**: Track successful transaction completion rate

#### `Nanostore.transaction.rollback.total` (Counter)
- **Description**: Total number of transaction rollbacks
- **Type**: Counter
- **When recorded**: When `Transaction::rollback()` is called
- **Use case**: Track rollback rate

#### `Nanostore.transaction.abort.total` (Counter)
- **Description**: Total number of transaction aborts by reason
- **Type**: Counter
- **Labels**: 
  - `reason`: One of `invalid_state`, `write_write_conflict`, `read_write_conflict`, `serialization_conflict`, `not_found`, `deadlock`, `other`
- **When recorded**: When a transaction error occurs
- **Use case**: Understand why transactions are failing

### Active Transaction Metrics

#### `Nanostore.transaction.active` (Gauge)
- **Description**: Number of currently active transactions
- **Type**: Gauge
- **When recorded**: Incremented on transaction begin, decremented on commit/rollback
- **Use case**: Monitor concurrent transaction load

### Latency Metrics

#### `Nanostore.transaction.duration_seconds` (Histogram)
- **Description**: Total transaction duration from begin to commit/rollback
- **Type**: Histogram
- **Unit**: Seconds (as f64)
- **When recorded**: On transaction commit or rollback
- **Use case**: Analyze transaction execution time, identify slow transactions

#### `Nanostore.transaction.commit.duration_seconds` (Histogram)
- **Description**: Time spent in the commit phase (two-phase commit execution)
- **Type**: Histogram
- **Unit**: Seconds (as f64)
- **When recorded**: On successful commit
- **Use case**: Analyze commit overhead, identify commit bottlenecks

#### `Nanostore.transaction.rollback.duration_seconds` (Histogram)
- **Description**: Time spent in the rollback phase
- **Type**: Histogram
- **Unit**: Seconds (as f64)
- **When recorded**: On rollback
- **Use case**: Analyze rollback overhead

### Conflict Metrics

#### `Nanostore.transaction.conflict.write_write` (Counter)
- **Description**: Number of write-write conflicts detected
- **Type**: Counter
- **When recorded**: When `ConflictDetector::check_write_conflict()` detects a conflict
- **Use case**: Monitor contention on specific keys

#### `Nanostore.transaction.conflict.read_write` (Counter)
- **Description**: Number of read-write conflicts detected (serializable isolation)
- **Type**: Counter
- **When recorded**: When `ConflictDetector::check_read_write_conflicts()` detects a conflict
- **Use case**: Monitor serialization conflicts

### Deadlock Metrics

#### `Nanostore.transaction.deadlock.detected` (Counter)
- **Description**: Number of deadlocks detected
- **Type**: Counter
- **When recorded**: When `DeadlockDetector::detect_cycle()` finds a cycle
- **Use case**: Monitor deadlock frequency, tune lock acquisition strategies

## Tracing Spans

The transaction layer uses structured tracing for detailed execution visibility.

### Transaction Lifecycle Span

**Span**: `Transaction::new`
- **Level**: Debug
- **Fields**:
  - `txn_id`: Transaction ID
  - `isolation`: Isolation level (Debug format)
- **Events**:
  - "Transaction started" (debug level)

### Commit Span

**Span**: `Transaction::commit`
- **Level**: Info
- **Fields**:
  - `txn_id`: Transaction ID
  - `write_count`: Number of writes in the transaction
- **Events**:
  - "Transaction committed successfully" (info level)
    - `duration_ms`: Total transaction duration in milliseconds
    - `commit_duration_ms`: Commit phase duration in milliseconds
    - `write_count`: Number of writes committed

### Rollback Span

**Span**: `Transaction::rollback`
- **Level**: Info
- **Fields**:
  - `txn_id`: Transaction ID
- **Events**:
  - "Transaction rolled back" (info level)
    - `duration_ms`: Total transaction duration in milliseconds
    - `rollback_duration_ms`: Rollback phase duration in milliseconds

## Usage Examples

### Prometheus Query Examples

```promql
# Transaction throughput (commits per second)
rate(Nanostore_transaction_commit_total[5m])

# Transaction failure rate
rate(Nanostore_transaction_abort_total[5m])

# Average transaction duration
histogram_quantile(0.95, rate(Nanostore_transaction_duration_seconds_bucket[5m]))

# Conflict rate
rate(Nanostore_transaction_conflict_write_write[5m]) + 
rate(Nanostore_transaction_conflict_read_write[5m])

# Active transaction count
Nanostore_transaction_active

# Deadlock rate
rate(Nanostore_transaction_deadlock_detected[5m])
```

### Tracing Query Examples

Using a tracing backend like Jaeger or Tempo:

```
# Find slow transactions
span.duration > 1s AND span.name = "Transaction::commit"

# Find transactions with many writes
span.write_count > 1000

# Find failed transactions
span.status = error AND span.name LIKE "Transaction::%"
```

## Implementation Details

### Timing

- Transaction start time is captured in `Transaction::start_time` field (Instant)
- Commit/rollback start times are captured at the beginning of those methods
- Durations are calculated using `Instant::elapsed()` for accuracy

### Metric Recording

- Counters are incremented using `metrics::counter!().increment(1)`
- Histograms record durations using `metrics::histogram!().record(duration.as_secs_f64())`
- Gauges are updated using `metrics::gauge!().increment(1.0)` and `.decrement(1.0)`

### Error Tracking

The `TransactionError::record_abort_metric()` method automatically records abort metrics with the appropriate reason label based on the error variant.

## Performance Considerations

- Metrics recording has minimal overhead (typically < 1μs per metric)
- Tracing spans use lazy evaluation and are only materialized when a subscriber is active
- `Instant` timing uses monotonic clocks and is not affected by system time changes
- No heap allocations occur during normal metric recording

## Future Enhancements

Potential additions for future versions:

1. **Lock wait timing**: Track time spent waiting for locks
2. **Retry metrics**: Count and time transaction retries
3. **Snapshot metrics**: Track active snapshot count and age
4. **Write set size**: Histogram of write set sizes
5. **Read set size**: Histogram of read set sizes (for serializable isolation)
6. **Conflict key tracking**: Identify hot keys causing conflicts
7. **Transaction type labels**: Distinguish read-only vs read-write transactions

## Related Documentation

- [METRICS_AND_OBSERVABILITY.md](METRICS_AND_OBSERVABILITY.md) - Overall observability strategy
- [ISOLATION_LEVELS.md](ISOLATION_LEVELS.md) - Transaction isolation levels
- [TWO_PHASE_COMMIT_IMPLEMENTATION.md](TWO_PHASE_COMMIT_IMPLEMENTATION.md) - Commit protocol details