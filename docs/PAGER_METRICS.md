# Pager Layer Metrics and Tracing

This document describes the observability instrumentation for the pager layer in Nanostore.

## Overview

The pager layer includes comprehensive metrics and tracing to monitor page I/O, cache performance, allocation/deallocation, compression/encryption overhead, and lock contention. This enables production monitoring, debugging, and performance analysis.

## Metrics

All metrics use the `Nanostore.pager.*` namespace.

### Page Lifecycle Metrics

#### `Nanostore.pager.page.allocated` (Counter)
- **Description**: Total number of pages allocated
- **Type**: Counter
- **When recorded**: When `Pager::allocate_page()` completes successfully
- **Use case**: Track page allocation rate

#### `Nanostore.pager.page.freed` (Counter)
- **Description**: Total number of pages freed
- **Type**: Counter
- **When recorded**: When `Pager::free_page()` completes successfully
- **Use case**: Track page deallocation rate

#### `Nanostore.pager.page.reused` (Counter)
- **Description**: Number of pages reused from free list
- **Type**: Counter
- **When recorded**: When `allocate_page()` gets a page from the free list
- **Use case**: Monitor free list effectiveness

#### `Nanostore.pager.page.grown` (Counter)
- **Description**: Number of new pages allocated (database growth)
- **Type**: Counter
- **When recorded**: When `allocate_page()` allocates a new page (not from free list)
- **Use case**: Track database file growth

### Page I/O Metrics

#### `Nanostore.pager.page.read` (Counter)
- **Description**: Total number of page reads
- **Type**: Counter
- **When recorded**: When `Pager::read_page()` completes successfully
- **Use case**: Track read I/O volume

#### `Nanostore.pager.page.write` (Counter)
- **Description**: Total number of page writes
- **Type**: Counter
- **When recorded**: When `Pager::write_page()` completes successfully
- **Use case**: Track write I/O volume

#### `Nanostore.pager.bytes.read` (Counter)
- **Description**: Total bytes read from disk
- **Type**: Counter
- **Unit**: Bytes
- **When recorded**: After each successful page read
- **Use case**: Monitor I/O bandwidth usage

#### `Nanostore.pager.bytes.written` (Counter)
- **Description**: Total bytes written to disk
- **Type**: Counter
- **Unit**: Bytes
- **When recorded**: After each successful page write
- **Use case**: Monitor I/O bandwidth usage

### Latency Metrics

#### `Nanostore.pager.allocate.duration_seconds` (Histogram)
- **Description**: Time to allocate a page
- **Type**: Histogram
- **Unit**: Seconds (as f64)
- **When recorded**: On page allocation completion
- **Use case**: Identify allocation bottlenecks

#### `Nanostore.pager.free.duration_seconds` (Histogram)
- **Description**: Time to free a page
- **Type**: Histogram
- **Unit**: Seconds (as f64)
- **When recorded**: On page free completion
- **Use case**: Identify deallocation bottlenecks

#### `Nanostore.pager.read.duration_seconds` (Histogram)
- **Description**: Time to read a page (including cache lookup)
- **Type**: Histogram
- **Unit**: Seconds (as f64)
- **When recorded**: On page read completion
- **Use case**: Analyze read latency, identify slow reads

#### `Nanostore.pager.write.duration_seconds` (Histogram)
- **Description**: Time to write a page (including cache update)
- **Type**: Histogram
- **Unit**: Seconds (as f64)
- **When recorded**: On page write completion
- **Use case**: Analyze write latency, identify slow writes

#### `Nanostore.pager.fsync.duration_seconds` (Histogram)
- **Description**: Time spent in fsync operations
- **Type**: Histogram
- **Unit**: Seconds (as f64)
- **When recorded**: After each fsync call
- **Use case**: Monitor disk sync overhead

### Cache Metrics

#### `Nanostore.pager.cache.hit` (Counter)
- **Description**: Number of cache hits
- **Type**: Counter
- **When recorded**: When `PageCache::get()` finds a page
- **Use case**: Calculate cache hit rate

#### `Nanostore.pager.cache.miss` (Counter)
- **Description**: Number of cache misses
- **Type**: Counter
- **When recorded**: When `PageCache::get()` doesn't find a page
- **Use case**: Calculate cache miss rate

#### `Nanostore.pager.cache.eviction` (Counter)
- **Description**: Number of pages evicted from cache
- **Type**: Counter
- **When recorded**: When LRU eviction occurs
- **Use case**: Monitor cache pressure

#### `Nanostore.pager.cache.dirty_flush` (Counter)
- **Description**: Number of dirty pages flushed to disk
- **Type**: Counter
- **When recorded**: When a dirty page is written to disk
- **Use case**: Monitor write-back cache behavior

#### `Nanostore.pager.cache.size` (Gauge)
- **Description**: Current number of pages in cache
- **Type**: Gauge
- **When recorded**: After cache operations
- **Use case**: Monitor cache utilization

#### `Nanostore.pager.cache.dirty_pages` (Gauge)
- **Description**: Current number of dirty pages in cache
- **Type**: Gauge
- **When recorded**: After cache operations
- **Use case**: Monitor write-back cache state

### Free List Metrics

#### `Nanostore.pager.freelist.size` (Gauge)
- **Description**: Current number of pages in free list
- **Type**: Gauge
- **When recorded**: After allocation/deallocation
- **Use case**: Monitor free space availability

#### `Nanostore.pager.freelist.push` (Counter)
- **Description**: Number of pages added to free list
- **Type**: Counter
- **When recorded**: When `FreeList::push_page()` is called
- **Use case**: Track free list additions

#### `Nanostore.pager.freelist.pop` (Counter)
- **Description**: Number of pages removed from free list
- **Type**: Counter
- **When recorded**: When `FreeList::pop_page()` returns a page
- **Use case**: Track free list usage

### Compression/Encryption Metrics

#### `Nanostore.pager.compression.duration_seconds` (Histogram)
- **Description**: Time spent compressing pages
- **Type**: Histogram
- **Unit**: Seconds (as f64)
- **When recorded**: During page serialization with compression
- **Use case**: Monitor compression overhead

#### `Nanostore.pager.decompression.duration_seconds` (Histogram)
- **Description**: Time spent decompressing pages
- **Type**: Histogram
- **Unit**: Seconds (as f64)
- **When recorded**: During page deserialization with compression
- **Use case**: Monitor decompression overhead

#### `Nanostore.pager.encryption.duration_seconds` (Histogram)
- **Description**: Time spent encrypting pages
- **Type**: Histogram
- **Unit**: Seconds (as f64)
- **When recorded**: During page serialization with encryption
- **Use case**: Monitor encryption overhead

#### `Nanostore.pager.decryption.duration_seconds` (Histogram)
- **Description**: Time spent decrypting pages
- **Type**: Histogram
- **Unit**: Seconds (as f64)
- **When recorded**: During page deserialization with encryption
- **Use case**: Monitor decryption overhead

#### `Nanostore.pager.compression.ratio` (Histogram)
- **Description**: Compression ratio achieved (original_size / compressed_size)
- **Type**: Histogram
- **When recorded**: After each compression
- **Use case**: Analyze compression effectiveness

### Error Metrics

#### `Nanostore.pager.error` (Counter)
- **Description**: Number of pager errors by type
- **Type**: Counter
- **Labels**:
  - `type`: One of `page_not_found`, `page_pinned`, `invalid_page_id`, `checksum_mismatch`, `io_error`, `other`
- **When recorded**: When a pager error occurs
- **Use case**: Monitor error rates and types

### Lock Contention Metrics

#### `Nanostore.pager.lock.wait_seconds` (Histogram)
- **Description**: Time spent waiting for locks
- **Type**: Histogram
- **Unit**: Seconds (as f64)
- **Labels**:
  - `lock_type`: One of `page_table`, `cache`, `file`, `superblock`, `header`
- **When recorded**: After acquiring a lock
- **Use case**: Identify lock contention bottlenecks

## Tracing Spans

The pager layer uses structured tracing for detailed execution visibility.

### Page Allocation Span

**Span**: `Pager::allocate_page`
- **Level**: Debug
- **Fields**:
  - `page_type`: Type of page being allocated
  - `page_id`: Allocated page ID (recorded on completion)
  - `from_freelist`: Whether page came from free list (recorded on completion)
- **Events**:
  - "Allocating page" (debug level)
  - "Page allocated from free list" or "Page allocated by growing database" (debug level)

### Page Free Span

**Span**: `Pager::free_page`
- **Level**: Debug
- **Fields**:
  - `page_id`: Page ID being freed
- **Events**:
  - "Freeing page" (debug level)
  - "Page freed successfully" (debug level)

### Page Read Span

**Span**: `Pager::read_page`
- **Level**: Debug
- **Fields**:
  - `page_id`: Page ID being read
  - `cache_hit`: Whether page was found in cache (recorded on completion)
- **Events**:
  - "Reading page" (debug level)
  - "Cache hit" or "Cache miss - reading from disk" (debug level)
  - "Page read successfully" (debug level)

### Page Write Span

**Span**: `Pager::write_page`
- **Level**: Debug
- **Fields**:
  - `page_id`: Page ID being written
  - `write_through`: Whether using write-through cache mode
- **Events**:
  - "Writing page" (debug level)
  - "Page written successfully" (debug level)

### Flush Span

**Span**: `Pager::flush_dirty_pages`
- **Level**: Info
- **Fields**:
  - `dirty_count`: Number of dirty pages to flush
- **Events**:
  - "Flushing dirty pages" (info level)
  - "Dirty pages flushed" (info level)
    - `flushed_count`: Number of pages actually flushed
    - `duration_ms`: Flush duration in milliseconds

### Sync Span

**Span**: `Pager::sync`
- **Level**: Info
- **Events**:
  - "Syncing to disk" (info level)
  - "Sync completed" (info level)
    - `duration_ms`: Sync duration in milliseconds

## Usage Examples

### Prometheus Query Examples

```promql
# Page read throughput (pages per second)
rate(Nanostore_pager_page_read[5m])

# Page write throughput (pages per second)
rate(Nanostore_pager_page_write[5m])

# Cache hit rate
rate(Nanostore_pager_cache_hit[5m]) / 
(rate(Nanostore_pager_cache_hit[5m]) + rate(Nanostore_pager_cache_miss[5m]))

# Average read latency (95th percentile)
histogram_quantile(0.95, rate(Nanostore_pager_read_duration_seconds_bucket[5m]))

# Average write latency (95th percentile)
histogram_quantile(0.95, rate(Nanostore_pager_write_duration_seconds_bucket[5m]))

# I/O bandwidth (bytes per second)
rate(Nanostore_pager_bytes_read[5m]) + rate(Nanostore_pager_bytes_written[5m])

# Free list size
Nanostore_pager_freelist_size

# Cache utilization
Nanostore_pager_cache_size

# Error rate
rate(Nanostore_pager_error[5m])

# Compression effectiveness
histogram_quantile(0.5, rate(Nanostore_pager_compression_ratio_bucket[5m]))
```

### Tracing Query Examples

Using a tracing backend like Jaeger or Tempo:

```
# Find slow page reads
span.duration > 100ms AND span.name = "Pager::read_page"

# Find cache misses
span.cache_hit = false AND span.name = "Pager::read_page"

# Find large flushes
span.dirty_count > 100 AND span.name = "Pager::flush_dirty_pages"

# Find failed operations
span.status = error AND span.name LIKE "Pager::%"
```

## Implementation Details

### Timing

- Operation start times are captured using `Instant::now()` at the beginning of each operation
- Durations are calculated using `Instant::elapsed()` for accuracy
- Lock wait times are measured by capturing time before and after lock acquisition

### Metric Recording

- Counters are incremented using `metrics::counter!().increment(1)`
- Histograms record durations using `metrics::histogram!().record(duration.as_secs_f64())`
- Gauges are updated using `metrics::gauge!().set(value as f64)`
- Labels are added using the `"key" => "value"` syntax

### Tracing Integration

- All public pager methods use `#[instrument]` attribute for automatic span creation
- Span fields are added using the `fields()` parameter
- Events are recorded using `tracing::debug!()`, `tracing::info!()`, etc.
- Error spans are automatically marked with `error` status

## Performance Considerations

- Metrics recording has minimal overhead (typically < 1μs per metric)
- Tracing spans use lazy evaluation and are only materialized when a subscriber is active
- `Instant` timing uses monotonic clocks and is not affected by system time changes
- No heap allocations occur during normal metric recording
- Lock timing adds minimal overhead (one additional `Instant::now()` call per lock acquisition)

## Related Documentation

- [TRANSACTION_METRICS.md](TRANSACTION_METRICS.md) - Transaction layer metrics
- [METRICS_AND_OBSERVABILITY.md](METRICS_AND_OBSERVABILITY.md) - Overall observability strategy
- [PAGER_CONCURRENCY_COMPLETE.md](PAGER_CONCURRENCY_COMPLETE.md) - Pager concurrency design