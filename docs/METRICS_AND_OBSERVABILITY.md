# Metrics and Observability

Nanostore includes comprehensive metrics and tracing instrumentation for production monitoring and performance analysis.

## Overview

The library uses:
- **`metrics`** crate for counters, gauges, and histograms
- **`tracing`** crate for structured logging and spans
- **`tracing-timing`** crate for latency percentile tracking

As a library, Nanostore does **not** include a metrics exporter. Users must configure their own exporter in their application code.

## Instrumented Components

### Pager Metrics

**Counters:**
- `pager.page_read` - Number of page reads
- `pager.page_write` - Number of page writes
- `pager.page_allocated` - Number of pages allocated
- `pager.page_freed` - Number of pages freed
- `pager.error` - Errors (with `type` label)

**Histograms:**
- `pager.read_duration` - Page read latency (seconds)
- `pager.write_duration` - Page write latency (seconds)
- `pager.allocate_duration` - Page allocation latency (seconds)
- `pager.free_duration` - Page free latency (seconds)
- `pager.sync_duration` - Sync operation latency (seconds)

**Gauges:**
- `pager.cache.size` - Current cache size (entries)
- `pager.cache.dirty_pages` - Number of dirty pages in cache
- `pager.cache.hit_rate` - Cache hit rate (0.0 to 1.0)

### Cache Metrics

**Counters:**
- `cache.hit` - Cache hits
- `cache.miss` - Cache misses
- `cache.eviction` - Cache evictions

**Gauges:**
- `cache.size` - Current cache size (entries)
- `cache.dirty_pages` - Number of dirty pages
- `cache.hit_rate` - Hit rate (0.0 to 1.0)

### LSM Tree Metrics

**Counters:**
- `lsm.memtable_write` - Number of memtable writes
- `lsm.memtable_flush` - Number of memtable flushes
- `lsm.sstable_read` - Number of SSTable reads
- `lsm.compaction` - Number of compaction operations
- `lsm.error` - Errors (with `type` label)

**Histograms:**
- `lsm.get_duration` - Get operation latency (seconds)
- `lsm.write_duration` - Write operation latency (seconds)
- `lsm.compaction_duration` - Compaction latency (seconds)
- `lsm.flush_duration` - Memtable flush latency (seconds)

**Gauges:**
- `lsm.sstable_count` - Number of SSTables
- `lsm.bloom_false_positive_rate` - Bloom filter false positive rate

### BTree Metrics

**Counters:**
- `btree.node_read` - Number of node reads
- `btree.node_write` - Number of node writes

**Histograms:**
- `btree.read_duration` - Node read latency (seconds)
- `btree.write_duration` - Node write latency (seconds)
- `btree.search_duration` - Search operation latency (seconds)

### WAL Metrics

**Counters:**
- `wal.write` - Number of WAL writes
- `wal.sync` - Number of sync operations
- `wal.bytes_written` - Total bytes written
- `wal.error` - Errors (with `type` label)

**Histograms:**
- `wal.write_duration` - Write latency (seconds)
- `wal.sync_duration` - Sync latency (seconds)

**Gauges:**
- `wal.size_bytes` - Current WAL file size
- `wal.active_transactions` - Number of active transactions

## Setting Up Metrics Collection

### Using Prometheus

```rust
use metrics_exporter_prometheus::PrometheusBuilder;

fn main() {
    // Install Prometheus exporter
    let builder = PrometheusBuilder::new();
    builder
        .install()
        .expect("failed to install Prometheus recorder");

    // Now use Nanostore - metrics will be collected automatically
    // ...
}
```

### Using Other Exporters

Nanostore works with any `metrics`-compatible exporter:

- **Prometheus**: `metrics-exporter-prometheus`
- **StatsD**: `metrics-exporter-statsd`
- **CloudWatch**: `metrics-exporter-cloudwatch`
- **Datadog**: `metrics-exporter-datadog`

## Setting Up Tracing

### Basic Tracing Setup

```rust
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

fn main() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Now use Nanostore - traces will be logged
    // ...
}
```

### With Timing Information

```rust
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use tracing_timing::{Builder, Histogram};

fn main() {
    let timing_layer = Builder::default()
        .layer(|| Histogram::new_with_max(1_000_000, 2).unwrap());

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(timing_layer)
        .init();

    // Now use Nanostore - timing data will be collected
    // ...
}
```

## Instrumented Operations

### Pager Operations

All major pager operations are instrumented with:
- Tracing spans (with `#[instrument]`)
- Latency histograms
- Operation counters
- Error tracking

Example trace output:
```
DEBUG pager::pagefile: Creating new pager path="test.db"
DEBUG pager::pagefile: Allocating page page_type=BTreeLeaf
DEBUG pager::pagefile: Reading page page_id=2
DEBUG pager::pagefile: Writing page page_id=2
```

### WAL Operations

WAL operations include:
- Transaction lifecycle tracking
- Write operation metrics
- Sync latency measurement
- Active transaction gauges

Example trace output:
```
DEBUG wal::writer: Creating new WAL writer path="test.wal"
DEBUG wal::writer: Writing BEGIN record txn_id=1
DEBUG wal::writer: Writing WRITE record txn_id=1 table_id=1 key_len=5 value_len=10
DEBUG wal::writer: Writing COMMIT record txn_id=1
```

## Monitoring Best Practices

### Key Metrics to Monitor

1. **Latency Percentiles**
   - p50, p95, p99 for read/write operations
   - Alert on p99 > threshold

2. **Cache Performance**
   - Hit rate should be > 80% for good performance
   - Monitor eviction rate

3. **WAL Performance**
   - Sync latency impacts write throughput
   - Monitor active transaction count

4. **Error Rates**
   - Track error types and frequencies
   - Alert on error rate spikes

### Example Prometheus Queries

```promql
# Average read latency (p50)
histogram_quantile(0.5, rate(pager_read_duration_bucket[5m]))

# Cache hit rate
rate(cache_hit[5m]) / (rate(cache_hit[5m]) + rate(cache_miss[5m]))

# WAL sync latency (p99)
histogram_quantile(0.99, rate(wal_sync_duration_bucket[5m]))

# Active transactions
wal_active_transactions
```

## Performance Impact

The metrics instrumentation has minimal overhead:
- Counters: ~10-20ns per increment
- Histograms: ~50-100ns per record
- Tracing spans: ~100-200ns per span (when enabled)

For production use:
- Metrics are always enabled (negligible overhead)
- Tracing can be disabled or filtered by level
- Use sampling for high-frequency operations if needed

## Disabling Instrumentation

While metrics are always collected, you can control tracing:

```rust
// Disable all tracing
std::env::set_var("RUST_LOG", "off");

// Enable only warnings and errors
std::env::set_var("RUST_LOG", "warn");

// Enable debug for specific modules
std::env::set_var("RUST_LOG", "Nanostore::pager=debug,Nanostore::wal=debug");
```

## Custom Metrics

Applications can add their own metrics alongside Nanostore's:

```rust
use metrics::{counter, histogram};

// Your application metrics
counter!("app.requests").increment(1);
histogram!("app.request_duration").record(duration.as_secs_f64());

// Nanostore metrics are collected automatically
db.write(key, value)?;
```

## Troubleshooting

### No Metrics Appearing

1. Ensure a metrics recorder is installed before using Nanostore
2. Check that the recorder is properly configured
3. Verify the metrics endpoint is accessible

### High Overhead

1. Reduce tracing verbosity (use `warn` or `error` level)
2. Use sampling for high-frequency operations
3. Consider using a more efficient metrics backend

### Missing Traces

1. Check `RUST_LOG` environment variable
2. Ensure tracing subscriber is initialized
3. Verify log level is appropriate for the operation

## See Also

- [metrics crate documentation](https://docs.rs/metrics/)
- [tracing crate documentation](https://docs.rs/tracing/)
- [tracing-timing documentation](https://docs.rs/tracing-timing/)