# Nanostore

[![License](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)
[![Actions Status](https://github.com/huhlig/nanostore/workflows/rust/badge.svg)](https://github.com/huhlig/nanostore/actions)

> Nanostore is a lightweight embeddable single-file database with ACID transactions, MVCC concurrency, and multiple storage engines.

## Features

- **Single-file database**: All data in one file for easy deployment
- **ACID transactions**: Full transaction support with snapshot isolation
- **Multiple storage engines**: 
  - BTree [Memory/Paged] (read-optimized)
  - LSM [Memory/Paged] (write-optimized)
- **MVCC concurrency**: Non-blocking reads with version chains
- **Write-ahead logging**: Crash recovery and durability
- **Configurable pages**: 4KB to 64KB page sizes
- **Optional compression**: LZ4 and Zstd support
- **Optional encryption**: AES-256-GCM encryption
- **Virtual file system**: Pluggable storage backends
- **Metrics & observability**: Built-in metrics and tracing for production monitoring

## Documentation

### Core Documentation

- **[Architecture Overview](docs/ARCHITECTURE.md)** - System architecture and component details
- **[File Format Specification](docs/FILE_FORMAT.md)** - Database and WAL file formats
- **[Metrics and Observability](docs/METRICS_AND_OBSERVABILITY.md)** - Production monitoring and performance tracking
- **[Architecture Decision Records](docs/adrs/)** - Key design decisions

### Key ADRs

- [ADR-001: Single-File Database Design](docs/adrs/001-single-file-database.md)
- [ADR-003: MVCC Concurrency Control](docs/adrs/003-mvcc-concurrency.md)
- [ADR-004: Multiple Storage Engines](docs/adrs/004-multiple-storage-engines.md)
- [ADR-006: Sharded Concurrency Model](docs/adrs/006-sharded-concurrency.md)

### Implementation Details

- [Pager Concurrency](docs/PAGER_CONCURRENCY_COMPLETE.md)
- [Pager Lock Ordering](docs/PAGER_LOCK_ORDERING.md) - Deadlock prevention through lock hierarchy
- [Lock-Free FreeList](docs/LOCK_FREE_FREELIST_IMPLEMENTATION.md)
- [Sharded Cache](docs/SHARDED_CACHE_IMPLEMENTATION.md)
- [BTree Split/Merge](docs/BTREE_SPLIT_MERGE_IMPLEMENTATION.md)

## Quick Start

```rust
use nanostore::{Database, TableConfig, TableEngineKind};

// Create database
let db = Database::create("mydb.db")?;

// Create table
let config = TableConfig {
    engine: TableEngineKind::BTree,
    name: "users".to_string(),
};
let table = db.create_table(config)?;

// Start transaction
let mut txn = db.begin_transaction()?;

// Write data
txn.put(b"user:1", b"Alice")?;
txn.put(b"user:2", b"Bob")?;

// Commit
txn.commit()?;

// Read data
let txn = db.begin_transaction()?;
let value = txn.get(b"user:1")?;
assert_eq!(value, Some(b"Alice".to_vec()));
```

## Project Structure

```
nanostore/
├── docs/            # Documentation, Architecture, ADRs
├── src/
│   ├── vfs/         # Virtual File System
│   ├── wal/         # Write-Ahead Log
│   ├── pager/       # Page Management
│   ├── table/       # Storage Engines (BTree, LSM)
│   ├── txn/         # Transaction Management
│   └── ...
├── tests/           # Integration Tests
└── benches/         # Performance Benchmarks
```

---

## Testing

### Running Tests

```bash
# Run all tests
cargo test

# Run tests with output
cargo test -- --nocapture

# Run specific test module
cargo test --test pager_stress_tests
```

### Large-Scale Stress Tests

Nanostore includes comprehensive stress tests for 100K-200K pages that are marked with `#[ignore]` to avoid slowing down regular test runs.

```bash
# Run all large-scale stress tests
cargo test --test pager_stress_tests -- --ignored --nocapture

# Run specific stress test
cargo test --test pager_stress_tests test_sequential_allocation_100k_pages -- --ignored --nocapture
cargo test --test pager_stress_tests test_fragmentation_100k_pages -- --ignored --nocapture
cargo test --test pager_stress_tests test_memory_usage_200k_pages -- --ignored --nocapture
```

Available stress tests:
- `test_sequential_allocation_100k_pages` - Sequential allocation of 100K pages
- `test_mixed_allocation_deallocation_100k_pages` - Realistic mixed workload patterns
- `test_fragmentation_100k_pages` - Extreme fragmentation scenarios
- `test_memory_usage_200k_pages` - Scalability validation to 200K pages
- `test_free_list_chain_100k_pages` - Long free list chain performance
- `test_persistence_recovery_50k_pages` - Large database persistence

See [docs/PAGER_STRESS_TEST_RESULTS.md](docs/PAGER_STRESS_TEST_RESULTS.md) for detailed performance metrics.

### Benchmarks

```bash
# Run all benchmarks
cargo bench

# Run specific benchmark suite
cargo bench --bench pager_benchmarks
cargo bench --bench vfs_benchmarks
cargo bench --bench wal_benchmarks
```

---

## Contributing

Contributions are welcome! Please:

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/my-feature`)
3. Make your changes with tests
4. Submit a pull request

---

## License

Licensed under the [Apache License, Version 2.0](http://www.apache.org/licenses/LICENSE-2.0) or the
[MIT License](https://opensource.org/licenses/MIT), at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work
by you shall be dual-licensed as above, without any additional terms or conditions.

Copyright 2025–2026 Hans W. Uhlig. All Rights Reserved.