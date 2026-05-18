# Table Engine Metrics and Tracing

This document describes the comprehensive metrics and tracing instrumentation added to NanoKV's table engines.

## Overview

All table engines now have standardized metrics and tracing instrumentation to provide observability into their operations. The metrics follow a consistent naming convention and are organized by engine type.

## Metrics Module

Location: `src/table/metrics.rs`

This module provides:
- Common metrics for all table engines (get/put/delete/scan operations)
- Engine-specific metrics for specialized operations
- Helper functions for recording metrics
- Consistent naming conventions

### Naming Convention

All metrics follow the pattern: `nanokv.table.<engine>.<metric_name>`

Examples:
- `nanokv.table.btree.split` - BTree node split counter
- `nanokv.table.lsm.flush_duration` - LSM memtable flush latency
- `nanokv.table.bloom.false_positive_rate` - Bloom filter false positive rate

## Common Table Metrics

These metrics are available for all table engines:

### Counters
- `nanokv.table.<engine>.get` - Point lookup operations
- `nanokv.table.<engine>.put` - Write operations
- `nanokv.table.<engine>.delete` - Delete operations
- `nanokv.table.<engine>.scan` - Scan operations

### Histograms (Latency)
- `nanokv.table.<engine>.get_duration` - Get operation latency (seconds)
- `nanokv.table.<engine>.put_duration` - Put operation latency (seconds)
- `nanokv.table.<engine>.delete_duration` - Delete operation latency (seconds)
- `nanokv.table.<engine>.scan_duration` - Scan operation latency (seconds)

## BTree Metrics

### Counters
- `nanokv.table.btree.split` - Node split operations
- `nanokv.table.btree.merge` - Node merge operations
- `nanokv.table.btree.node_read` - Node read operations
- `nanokv.table.btree.node_write` - Node write operations

### Histograms
- `nanokv.table.btree.node_read_duration` - Node read latency
- `nanokv.table.btree.node_write_duration` - Node write latency
- `nanokv.table.btree.search_duration` - Search operation latency

### Gauges
- `nanokv.table.btree.tree_height` - Current tree height
- `nanokv.table.btree.internal_nodes` - Number of internal nodes
- `nanokv.table.btree.leaf_nodes` - Number of leaf nodes

### Implementation

BTree metrics are instrumented in:
- `split_node()` - Records split operations
- `merge_nodes()` - Records merge operations
- `read_node()` - Records node reads and latency
- `write_node()` - Records node writes and latency
- `search()` - Records search latency
- `stats()` - Updates tree structure gauges

## LSM Tree Metrics

### Counters
- `nanokv.table.lsm.memtable_write` - Memtable write operations
- `nanokv.table.lsm.memtable_flush` - Memtable flush operations
- `nanokv.table.lsm.sstable_read` - SSTable read operations
- `nanokv.table.lsm.compaction` - Compaction operations
- `nanokv.table.lsm.compaction_bytes_written` - Bytes written during compaction
- `nanokv.table.lsm.compaction_bytes_read` - Bytes read during compaction

### Histograms
- `nanokv.table.lsm.flush_duration` - Memtable flush latency
- `nanokv.table.lsm.compaction_duration` - Compaction latency

### Gauges
- `nanokv.table.lsm.sstable_count` - Number of SSTables
- `nanokv.table.lsm.memtable_size_bytes` - Current memtable size
- `nanokv.table.lsm.read_amplification` - Read amplification factor
- `nanokv.table.lsm.write_amplification` - Write amplification factor

### Implementation

LSM metrics are instrumented in:
- `get_internal()` - Records get operations and SSTable reads
- `put_internal()` - Records memtable writes
- `rotate_memtable()` - Records memtable flushes
- `flush_memtable()` - Records flush duration
- Compaction operations (future enhancement)

## Bloom Filter Metrics

### Counters
- `nanokv.table.bloom.insert` - Insert operations
- `nanokv.table.bloom.query` - Query operations
- `nanokv.table.bloom.positive` - Positive query results (may contain)
- `nanokv.table.bloom.negative` - Negative query results (definitely not present)
- `nanokv.table.bloom.false_positive` - False positive detections

### Gauges
- `nanokv.table.bloom.saturation` - Filter saturation level (0.0 to 1.0)
- `nanokv.table.bloom.false_positive_rate` - Estimated false positive rate
- `nanokv.table.bloom.bits_set` - Number of bits set in the filter
- `nanokv.table.bloom.total_bits` - Total number of bits in the filter

### Implementation Status

Bloom filter metrics are defined but not yet instrumented. Future work includes:
- Adding metrics to insert operations
- Tracking query results and false positives
- Calculating and updating saturation levels

## RTree Metrics

### Counters
- `nanokv.table.rtree.split` - Node split operations
- `nanokv.table.rtree.query` - Spatial query operations

### Histograms
- `nanokv.table.rtree.query_candidates` - Number of candidate nodes examined
- `nanokv.table.rtree.query_results` - Number of results returned
- `nanokv.table.rtree.query_duration` - Query latency

### Gauges
- `nanokv.table.rtree.tree_height` - Current tree height
- `nanokv.table.rtree.object_count` - Number of objects indexed

### Implementation Status

RTree metrics are defined but not yet instrumented. The RTree already has tracing spans but needs metric calls added.

## TimeSeries Metrics

### Counters
- `nanokv.table.timeseries.append` - Data point append operations
- `nanokv.table.timeseries.bucket_created` - Bucket creation operations
- `nanokv.table.timeseries.bucket_flushed` - Bucket flush operations
- `nanokv.table.timeseries.aggregation` - Aggregation query operations

### Histograms
- `nanokv.table.timeseries.aggregation_points` - Number of points in aggregation
- `nanokv.table.timeseries.append_duration` - Append operation latency
- `nanokv.table.timeseries.aggregation_duration` - Aggregation query latency

### Gauges
- `nanokv.table.timeseries.active_buckets` - Number of active buckets
- `nanokv.table.timeseries.total_points` - Total data points stored
- `nanokv.table.timeseries.compression_ratio` - Compression ratio achieved

### Implementation Status

TimeSeries metrics are defined but not yet instrumented.

## HNSW Vector Index Metrics

### Counters
- `nanokv.table.hnsw.insert` - Vector insert operations
- `nanokv.table.hnsw.search` - Vector search operations

### Histograms
- `nanokv.table.hnsw.distance_calculations` - Distance calculations during search
- `nanokv.table.hnsw.search_hops` - Number of hops during search
- `nanokv.table.hnsw.search_duration` - Search latency

### Gauges
- `nanokv.table.hnsw.vector_count` - Number of vectors indexed
- `nanokv.table.hnsw.max_layer` - Maximum layer in the graph

### Implementation Status

HNSW metrics are defined but not yet instrumented.

## Tracing Instrumentation

All major table operations use the `#[instrument]` attribute for tracing:

### BTree Tracing
- `split_node()` - Node split operations with page_id
- `merge_nodes()` - Node merge operations with left/right page IDs
- `read_node()` - Node reads with page_id
- `write_node()` - Node writes with page_id
- `search()` - Search operations with key length

### LSM Tracing
- `get_internal()` - Get operations with key length
- `put_internal()` - Put operations with key/value lengths
- `flush_memtable()` - Memtable flush operations

### Span Fields

Tracing spans include relevant context:
- `page_id` - Page identifier for disk operations
- `key_len` - Key length for lookups
- `value_len` - Value length for writes
- `from_freelist` - Whether page was reused

## Usage Examples

### Recording Common Metrics

```rust
use crate::table::metrics;
use std::time::Instant;

// Record a get operation
let start = Instant::now();
let result = table.get(key)?;
metrics::record_get("btree");
metrics::record_get_duration("btree", start);
```

### Recording Engine-Specific Metrics

```rust
use crate::table::metrics;

// BTree split
metrics::btree::record_split();
metrics::btree::set_tree_height(5);

// LSM flush
let start = Instant::now();
flush_memtable()?;
metrics::lsm::record_memtable_flush();
metrics::lsm::record_flush_duration(start);

// Bloom filter query
metrics::bloom::record_query();
if may_contain {
    metrics::bloom::record_positive();
} else {
    metrics::bloom::record_negative();
}
```

### Adding Tracing Spans

```rust
use tracing::instrument;

#[instrument(skip(self, key), fields(key_len = key.len()))]
fn search(&self, key: &[u8]) -> Result<Value> {
    // Implementation
}
```

## Monitoring Recommendations

### Key Metrics to Monitor

1. **Latency Percentiles**
   - p50, p95, p99 for all operation types
   - Alert on p99 > threshold

2. **BTree Health**
   - Tree height (should grow slowly)
   - Split/merge ratio (balanced tree)
   - Node read/write ratio

3. **LSM Performance**
   - Read/write amplification factors
   - Memtable flush frequency
   - Compaction duration and frequency
   - SSTable count per level

4. **Bloom Filter Effectiveness**
   - False positive rate (should be < 1%)
   - Saturation level (rebuild if > 80%)
   - Query hit rate

### Example Prometheus Queries

```promql
# BTree p99 search latency
histogram_quantile(0.99, rate(nanokv_table_btree_search_duration_bucket[5m]))

# LSM read amplification
nanokv_table_lsm_read_amplification

# Bloom filter false positive rate
rate(nanokv_table_bloom_false_positive[5m]) / rate(nanokv_table_bloom_positive[5m])

# RTree query efficiency (results per candidate examined)
rate(nanokv_table_rtree_query_results[5m]) / rate(nanokv_table_rtree_query_candidates[5m])
```

## Performance Impact

The metrics instrumentation has minimal overhead:
- Counters: ~10-20ns per increment
- Histograms: ~50-100ns per record
- Tracing spans: ~100-200ns per span (when enabled)

For production use:
- Metrics are always enabled (negligible overhead)
- Tracing can be filtered by level
- Use sampling for high-frequency operations if needed

## Future Enhancements

1. **Complete Instrumentation**
   - Add metrics to Bloom filter operations
   - Add metrics to RTree operations
   - Add metrics to TimeSeries operations
   - Add metrics to HNSW operations

2. **Advanced Metrics**
   - Cache hit rates per engine
   - Memory usage tracking
   - I/O bandwidth utilization
   - Lock contention metrics

3. **Compaction Metrics**
   - Detailed compaction statistics
   - Per-level compaction metrics
   - Compaction I/O tracking

4. **Aggregated Metrics**
   - Cross-engine statistics
   - Database-wide metrics
   - Performance trends

## Testing

The metrics module includes unit tests to verify:
- All metric functions compile and execute
- No panics or errors during metric recording
- Proper metric naming conventions

Run tests with:
```bash
cargo test table::metrics
```

## See Also

- [METRICS_AND_OBSERVABILITY.md](METRICS_AND_OBSERVABILITY.md) - Overall metrics strategy
- [PAGER_METRICS.md](PAGER_METRICS.md) - Pager layer metrics
- [TRANSACTION_METRICS.md](TRANSACTION_METRICS.md) - Transaction layer metrics