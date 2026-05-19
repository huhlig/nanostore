# LRU Page Cache Implementation

## Overview

The LRU (Least Recently Used) page cache is a critical performance optimization component in Nanostore's pager layer. It reduces disk I/O by keeping frequently accessed pages in memory, significantly improving read and write performance.

## Architecture

### Components

1. **PageCache** - Main cache interface with thread-safe operations
2. **CacheEntry** - Internal structure holding cached pages with metadata
3. **CacheStats** - Statistics tracking for cache performance monitoring
4. **CacheConfig** - Configuration for cache behavior

### Data Structures

The cache uses a combination of:
- **HashMap** for O(1) page lookups by PageId
- **Doubly-linked list** for O(1) LRU ordering and eviction
- **RwLock** for thread-safe concurrent access

```
┌─────────────────────────────────────────────────────────┐
│                      PageCache                          │
├─────────────────────────────────────────────────────────┤
│  HashMap<PageId, CacheEntry>                           │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐            │
│  │ Page 1   │←→│ Page 2   │←→│ Page 3   │            │
│  │ (MRU)    │  │          │  │ (LRU)    │            │
│  └──────────┘  └──────────┘  └──────────┘            │
│       ↑                              ↓                 │
│    lru_head                      lru_tail              │
└─────────────────────────────────────────────────────────┘
```

## Features

### 1. LRU Eviction Policy

When the cache reaches capacity, the least recently used page is evicted:

```rust
// Pages are moved to the front (MRU) on access
cache.get(page_id);  // Moves page to front

// When full, tail (LRU) is evicted
cache.put(new_page, dirty);  // May evict LRU page
```

### 2. Dirty Page Tracking

The cache tracks which pages have been modified:

```rust
// Mark page as dirty after modification
cache.put(page, true);  // dirty = true

// Check if page is dirty
if cache.is_dirty(page_id) {
    // Page needs to be written to disk
}

// Get all dirty pages for flushing
let dirty_pages = cache.get_dirty_pages();
```

### 3. Write-Back vs Write-Through

**Write-Back Mode** (default):
- Writes go to cache only
- Dirty pages written to disk on eviction or flush
- Better performance, higher risk of data loss

```rust
let config = PagerConfig::new()
    .with_cache_capacity(1000)
    .with_cache_write_back(true);  // Write-back mode
```

**Write-Through Mode**:
- Writes go to both cache and disk immediately
- Lower performance, better durability
- No dirty pages in cache

```rust
let config = PagerConfig::new()
    .with_cache_capacity(1000)
    .with_cache_write_back(false);  // Write-through mode
```

### 4. Cache Statistics

The cache tracks performance metrics:

```rust
let stats = pager.cache_stats().unwrap();
println!("Hit rate: {:.2}%", stats.hit_rate() * 100.0);
println!("Hits: {}, Misses: {}", stats.hits, stats.misses);
println!("Evictions: {}", stats.evictions);
println!("Dirty pages: {}", stats.dirty_pages);
```

## Configuration

### Cache Capacity

Set the maximum number of pages to cache:

```rust
let config = PagerConfig::new()
    .with_cache_capacity(1000);  // Cache up to 1000 pages
```

To disable caching:

```rust
let config = PagerConfig::new()
    .with_cache_capacity(0);  // No caching
```

### Write Policy

Choose between write-back and write-through:

```rust
// Write-back (better performance)
let config = PagerConfig::new()
    .with_cache_write_back(true);

// Write-through (better durability)
let config = PagerConfig::new()
    .with_cache_write_back(false);
```

## Integration with Pager

The cache is transparently integrated into the Pager:

### Read Path

```
read_page(page_id)
    ↓
Check cache
    ↓
┌─────────┬─────────┐
│ Hit     │ Miss    │
├─────────┼─────────┤
│ Return  │ Read    │
│ from    │ from    │
│ cache   │ disk    │
│         │    ↓    │
│         │ Add to  │
│         │ cache   │
│         │    ↓    │
│         │ Return  │
└─────────┴─────────┘
```

### Write Path (Write-Back)

```
write_page(page)
    ↓
Add to cache (dirty)
    ↓
Cache full?
    ↓
┌─────────┬─────────┐
│ No      │ Yes     │
├─────────┼─────────┤
│ Done    │ Evict   │
│         │ LRU     │
│         │    ↓    │
│         │ Dirty?  │
│         │    ↓    │
│         │ Write   │
│         │ to disk │
└─────────┴─────────┘
```

### Write Path (Write-Through)

```
write_page(page)
    ↓
Write to disk
    ↓
Add to cache (clean)
    ↓
Done
```

## Cache Operations

### Flushing

Flush all dirty pages to disk:

```rust
pager.flush_cache()?;
```

This is automatically called during:
- `pager.sync()` - Sync all changes
- `pager.clear_cache()` - Clear cache
- Pager drop (if implemented)

### Clearing

Remove all pages from cache:

```rust
pager.clear_cache()?;  // Flushes dirty pages first
```

### Statistics

Get cache performance metrics:

```rust
if let Some(stats) = pager.cache_stats() {
    println!("Cache size: {}/{}", stats.current_size, cache.capacity());
    println!("Hit rate: {:.2}%", stats.hit_rate() * 100.0);
    println!("Dirty pages: {}", stats.dirty_pages);
}
```

## Performance Characteristics

### Time Complexity

- **Cache hit**: O(1) - HashMap lookup + LRU list update
- **Cache miss**: O(1) cache operations + disk I/O
- **Eviction**: O(1) - Remove from tail of LRU list
- **Flush**: O(n) where n = number of dirty pages

### Space Complexity

- **Memory usage**: O(capacity × page_size)
- **Overhead**: ~48 bytes per cached page (entry metadata)

### Typical Performance

With a well-sized cache (working set fits in cache):
- **Read speedup**: 100-1000x (memory vs disk)
- **Write speedup**: 10-100x (write-back mode)
- **Hit rate**: 80-95% for typical workloads

## Best Practices

### 1. Size the Cache Appropriately

```rust
// Rule of thumb: 10-20% of database size or working set
let cache_size = database_pages / 10;
let config = PagerConfig::new()
    .with_cache_capacity(cache_size);
```

### 2. Monitor Cache Statistics

```rust
// Periodically check hit rate
let stats = pager.cache_stats().unwrap();
if stats.hit_rate() < 0.7 {
    // Consider increasing cache size
    eprintln!("Low cache hit rate: {:.2}%", stats.hit_rate() * 100.0);
}
```

### 3. Flush Before Critical Operations

```rust
// Before taking a backup
pager.flush_cache()?;
pager.sync()?;

// Before closing database
pager.flush_cache()?;
```

### 4. Choose Write Policy Based on Requirements

**Use write-back when:**
- Performance is critical
- Can tolerate some data loss risk
- Have proper shutdown procedures

**Use write-through when:**
- Durability is critical
- Cannot risk data loss
- Performance is acceptable

## Thread Safety

The cache is fully thread-safe:

```rust
let pager = Arc::new(pager);

// Multiple threads can safely access cache
let handles: Vec<_> = (0..4)
    .map(|_| {
        let pager = Arc::clone(&pager);
        thread::spawn(move || {
            pager.read_page(page_id).unwrap();
        })
    })
    .collect();
```

## Testing

Comprehensive tests are available in:
- `src/pager/cache.rs` - Unit tests
- `tests/cache_integration_tests.rs` - Integration tests

Run tests:
```bash
cargo test cache
```

## Benchmarks

Performance benchmarks are available in:
- `benches/cache_benchmarks.rs`

Run benchmarks:
```bash
cargo bench --bench cache_benchmarks
```

## Future Enhancements

Potential improvements:
1. **Adaptive cache sizing** - Dynamically adjust based on hit rate
2. **Page pinning** - Prevent eviction of critical pages
3. **Prefetching** - Anticipate future page accesses
4. **Multiple eviction policies** - LFU, ARC, etc.
5. **Cache warming** - Preload frequently accessed pages
6. **Compression** - Store compressed pages in cache

## References

- [LRU Cache Algorithm](https://en.wikipedia.org/wiki/Cache_replacement_policies#Least_recently_used_(LRU))
- [Write-Back vs Write-Through](https://en.wikipedia.org/wiki/Cache_(computing)#Writing_policies)
- [Database Buffer Pool Management](https://15445.courses.cs.cmu.edu/fall2023/notes/06-bufferpool.pdf)

---

**Made with Bob**