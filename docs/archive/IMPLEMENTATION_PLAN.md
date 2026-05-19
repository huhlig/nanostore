# Nanostore Implementation Plan

**Version**: 1.0  
**Date**: 2026-05-07  
**Status**: Draft

---

## Executive Summary

Nanostore is a lightweight embeddable single-file key-value storage engine designed as a **foundation layer for relational and graph databases**. It provides low-level storage primitives that higher-level database systems build upon.

**Key Design Principle**: Nanostore is NOT an end-user database. It is a storage engine providing:
- Efficient key-value storage with multiple table support
- ACID transactions at the storage layer (1:1 mapping with higher-level transactions)
- Pluggable storage engines (B-Tree, LSM)
- Page-level caching and management
- Write-ahead logging for durability

Higher-level systems (relational query engines, graph databases) use Nanostore's API to implement their own data models, query languages, and optimizations.

**Transaction Model**: Nanostore transactions map 1:1 to higher-level database transactions. The higher-level database begins a Nanostore transaction, performs all storage operations within it, then commits or rolls back. This avoids nested transactions and provides clean ACID semantics.

---

## Architecture Overview

### Layered Design
```
┌─────────────────────────────────────────────────┐
│  Higher-Level Database Layer                    │
│  (Relational Engine, Graph Engine, etc.)        │
│  - Query planning & execution                   │
│  - Schema management                            │
│  - Query language (SQL, Cypher, etc.)           │
└─────────────────────────────────────────────────┘
                      ↓ Uses Nanostore API
┌─────────────────────────────────────────────────┐
│  Nanostore Storage Engine                          │
├─────────────────────────────────────────────────┤
│  Core API (Transaction Management)              │
├─────────────────────────────────────────────────┤
│  Table Layer (B-Tree / LSM)                     │
├─────────────────────────────────────────────────┤
│  Cache + Index (Performance)                    │
├─────────────────────────────────────────────────┤
│  Pager (Page Management)                        │
├─────────────────────────────────────────────────┤
│  WAL (Write-Ahead Log)                          │
├─────────────────────────────────────────────────┤
│  VFS (Virtual File System)                      │
└─────────────────────────────────────────────────┘
```

### What Nanostore Provides

✅ **Storage Primitives**:
- Key-value get/put/delete operations
- Range scans and prefix scans
- Multi-table support (persistent and in-memory)
- ACID transactions
- Secondary indexes
- Crash recovery
- Memory tables (for temporary data, joins, intermediate results)

❌ **What Nanostore Does NOT Provide**:
- SQL query language
- Schema definitions (beyond key-value)
- Query planning/optimization
- Join operations
- User management
- Application-level features

---

## Implementation Phases

### Phase 1: Foundation (Weeks 1-3)
**VFS, Pager, WAL**

### Phase 2: Storage Engines (Weeks 4-6)
**B-Tree Table Implementation (Persistent & In-Memory)**

### Phase 3: Caching & Indexing (Weeks 7-8)
**Performance Optimizations**

### Phase 4: API & Transactions (Weeks 9-10)
**Storage Layer API**

### Phase 5: Optional Interfaces (Weeks 11-12)
**REST API & CLI (for debugging)**

### Phase 6: Production Readiness (Weeks 13-14)
**Testing & Documentation**

---

## Phase 1: Foundation (Weeks 1-3)

### 1.1 VFS Layer (Week 1)

**Objective**: Abstract filesystem operations for portability and testing.

**Tasks**:
- [ ] Define `VFS` trait with operations: open, read, write, sync, lock, truncate
- [ ] Implement `FileSystemVFS` using `std::fs`
- [ ] Implement `MemoryVFS` for testing
- [ ] Define error types
- [ ] Write comprehensive tests

**Deliverables**:
- `src/vfs/mod.rs`, `src/vfs/fs.rs`, `src/vfs/memory.rs`
- `tests/vfs_tests.rs`

---

### 1.2 Pager Layer (Week 2)

**Objective**: Implement block-level storage with page management.

**Design Decisions**:
- Page Size: 4096 bytes (4KB)
- Page Format: `[Header: 32B][Data: 4032B][Checksum: 32B]`
- Checksum: SHA-256 for integrity

**Tasks**:
- [ ] Define page format structures
- [ ] Implement `Pager` with allocate/free/read/write operations
- [ ] Implement free list management
- [ ] Add checksum validation
- [ ] Implement superblock
- [ ] Write tests

**Deliverables**:
- `src/pager/mod.rs`, `src/pager/page.rs`, `src/pager/superblock.rs`
- `tests/pager_tests.rs`

---

### 1.3 WAL (Write-Ahead Log) (Week 3)

**Objective**: Implement write-ahead logging for durability and crash recovery.

**Design Decisions**:
- WAL File: Separate file `<database>.wal`
- Record Types: BEGIN, WRITE, COMMIT, ROLLBACK, CHECKPOINT

**Tasks**:
- [ ] Define WAL record structures
- [ ] Implement `WalWriter` with buffered writes
- [ ] Implement `WalReader` for recovery
- [ ] Implement recovery logic
- [ ] Implement checkpointing
- [ ] Write tests

**Deliverables**:
- `src/wal/mod.rs`, `src/wal/writer.rs`, `src/wal/reader.rs`, `src/wal/recovery.rs`
- `tests/wal_tests.rs`

---

## Phase 2: Storage Engines (Weeks 4-6)

### 2.1 B-Tree Table (Weeks 4-6)

**Objective**: Implement B-Tree based key-value table optimized for higher-level database use.

**Design Decisions**:
- Node Size: Fits in one page (4KB)
- Order: ~100 keys per node
- Node Types: Internal (keys + pointers), Leaf (keys + values + next pointer)
- **Table Types**: Persistent (disk-backed) and In-Memory (RAM-only)

**Features for Higher-Level Systems**:
- Range scans (essential for joins and graph traversals)
- Prefix scans (useful for composite keys)
- Cursor support (streaming large result sets)
- Overflow pages (large values/BLOBs)
- **Memory tables** (for temporary data, join intermediate results, query execution)

**Tasks**:
- [ ] Define B-Tree node structures
- [ ] Implement node serialization with variable-length keys/values
- [ ] Implement insert with node splitting
- [ ] Implement get with binary search
- [ ] Implement delete with node merging
- [ ] Implement range_scan and prefix_scan
- [ ] Implement cursor abstraction
- [ ] **Implement in-memory B-Tree variant** (no disk I/O, faster operations)
- [ ] **Add table type configuration** (persistent vs in-memory)
- [ ] Write comprehensive tests

**Memory Table Features**:
```rust
pub enum TableType {
    Persistent,  // Disk-backed, survives restarts
    Memory,      // RAM-only, fast, for temporary data
}

pub struct TableConfig {
    pub table_type: TableType,
    pub engine: EngineType,
    // ... other config
}
```

**Use Cases for Memory Tables**:
- **Join Operations**: Store intermediate join results
- **Temporary Tables**: Query execution scratch space
- **Sorting**: In-memory sort buffers
- **Aggregations**: Temporary aggregation state
- **Query Planning**: Statistics and metadata

**Deliverables**:
- `src/table/btree/mod.rs`, `src/table/btree/node.rs`, `src/table/btree/cursor.rs`
- `src/table/btree/memory.rs` - In-memory B-Tree implementation
- `tests/btree_tests.rs`, `tests/memory_table_tests.rs`, `benches/btree_bench.rs`

---

## Phase 3: Caching & Indexing (Weeks 7-8)

### 3.1 Cache Layer (Week 7)

**Objective**: Implement LRU page cache for performance.

**Design Decisions**:
- Cache Size: Configurable (default 1000 pages = 4MB)
- Eviction Policy: LRU
- Dirty Tracking: Write-back policy

**Tasks**:
- [ ] Implement LRU cache with dirty page tracking
- [ ] Add cache statistics (hit rate, miss rate, evictions)
- [ ] Integrate with Pager
- [ ] Write tests

**Deliverables**:
- `src/cache/mod.rs`, `src/cache/lru.rs`, `src/cache/stats.rs`
- `tests/cache_tests.rs`

---

### 3.2 Index Support (Week 8)

**Objective**: Add secondary index support for higher-level query optimization.

**Use Cases**:
- Relational: Indexes on table columns for WHERE clauses
- Graph: Indexes on vertex/edge properties for filtering
- Document: Indexes on document fields for queries

**Tasks**:
- [ ] Define index abstraction (B-Tree based)
- [ ] Implement index operations: create, drop, lookup, scan
- [ ] Implement automatic index maintenance on table operations
- [ ] Add index consistency checks
- [ ] Write tests

**Deliverables**:
- `src/index/mod.rs`, `src/index/btree_index.rs`
- `tests/index_tests.rs`

---

## Phase 4: API & Transactions (Weeks 9-10)

### 4.1 Core API (Week 9)

**Objective**: Provide clean, safe API for database operations.

**API Philosophy**:
- **Low-level**: Expose storage primitives, not high-level abstractions
- **Flexible**: Allow higher-level systems to implement their own data models
- **Efficient**: Minimize overhead, support zero-copy where possible
- **Safe**: Prevent corruption, ensure ACID guarantees

**Transaction Model**:

Nanostore transactions are designed to be **controlled by the higher-level database**:

1. **1:1 Mapping**: One higher-level transaction = One Nanostore transaction
2. **No Nesting**: Nanostore doesn't support nested transactions
3. **Higher-Level Control**: The database layer manages transaction lifecycle
4. **ACID Guarantees**: Nanostore ensures atomicity, consistency, isolation, durability

**Key Types**:

```rust
pub struct Database {
    // Open/create database, manage tables, begin transactions
}

pub struct Transaction {
    // Multi-table ACID transactions (persistent and memory tables)
    // Controlled by higher-level database
    // All operations within this transaction are atomic
}

pub struct TransactionalTable {
    // get, put, delete, scan, prefix_scan operations
    // All operations are part of the parent transaction
}

pub enum TableType {
    Persistent,  // Disk-backed table
    Memory,      // In-memory table (for joins, temp data)
}

pub struct TableConfig {
    pub table_type: TableType,
    pub engine: EngineType,
    pub cache_size: Option<usize>,
    pub comparator: Option<Box<dyn Fn(&[u8], &[u8]) -> Ordering>>,
}
```

**Example Transaction Flow**:

```rust
// Higher-level database controls the transaction
fn execute_sql_transaction(db: &Database, statements: Vec<SqlStatement>) -> Result<()> {
    // Begin ONE Nanostore transaction for the entire SQL transaction
    let txn = db.begin_transaction()?;
    
    // Execute all SQL statements within this single transaction
    for stmt in statements {
        match stmt {
            SqlStatement::Insert(table, row) => {
                let storage_table = txn.table(&table)?;
                storage_table.put(&row.key(), &row.serialize())?;
            }
            SqlStatement::Update(table, key, row) => {
                let storage_table = txn.table(&table)?;
                storage_table.put(&key, &row.serialize())?;
            }
            SqlStatement::Delete(table, key) => {
                let storage_table = txn.table(&table)?;
                storage_table.delete(&key)?;
            }
        }
    }
    
    // Commit the entire transaction atomically
    txn.commit()?;
    Ok(())
}
```

**Tasks**:
- [ ] Define `Database` handle with table management
- [ ] Define `Transaction` handle with commit/rollback
- [ ] Define `TransactionalTable` with CRUD operations
- [ ] Implement isolation levels (read-committed default)
- [ ] Implement multi-table transactions (persistent and memory tables)
- [ ] **Add memory table lifecycle management** (auto-cleanup on transaction end)
- [ ] **Support mixing persistent and memory tables in same transaction**
- [ ] Write API documentation with examples
- [ ] Write tests

**Deliverables**:
- `src/api/mod.rs`, `src/api/database.rs`, `src/api/table.rs`, `src/api/transaction.rs`
- `examples/storage_layer_usage.rs`, `examples/relational_on_Nanostore.rs`, `examples/graph_on_Nanostore.rs`
- `tests/api_tests.rs`

---

### 4.2 Error Handling (Week 10)

**Objective**: Comprehensive error handling and recovery.

**Tasks**:
- [ ] Define error hierarchy
- [ ] Implement error recovery strategies
- [ ] Add corruption detection
- [ ] Implement recovery tools
- [ ] Write tests

**Deliverables**:
- `src/error.rs`, `src/recovery.rs`
- `tests/error_tests.rs`

---

## Phase 5: Optional Interfaces (Weeks 11-12)

**Note**: These are for debugging and management, NOT core functionality.

### 5.1 REST API (Week 11) - OPTIONAL

**Purpose**: HTTP interface for remote debugging and inspection.

**Endpoints**:
- `GET /tables` - List tables
- `GET /tables/:name/stats` - Table statistics
- `GET /cache/stats` - Cache statistics
- `POST /checkpoint` - Trigger checkpoint

**Tasks**:
- [ ] Implement REST handlers with Axum
- [ ] Add authentication
- [ ] Write tests

**Deliverables**:
- `src/rest/mod.rs`, `src/rest/handlers.rs`
- `tests/rest_tests.rs`

---

### 5.2 CLI Tool (Week 12) - OPTIONAL

**Purpose**: Command-line interface for database management and debugging.

**Commands**:
- `Nanostore create/open/table/get/put/delete/scan/inspect/check/repair`

**Tasks**:
- [ ] Implement CLI commands with Clap
- [ ] Implement inspection tools
- [ ] Write tests

**Deliverables**:
- `src/bin.rs`, `src/cli/mod.rs`
- `tests/cli_tests.rs`

---

## Phase 6: Production Readiness (Weeks 13-14)

### 6.1 Testing & Validation (Week 13)

**Tasks**:
- [ ] Add property-based tests (proptest)
- [ ] Add fuzzing (cargo-fuzz)
- [ ] Add stress tests
- [ ] Add benchmark suite (criterion)
- [ ] Test on multiple platforms

**Deliverables**:
- `tests/property_tests.rs`, `fuzz/`, `tests/stress_tests.rs`, `benches/`

---

### 6.2 Documentation (Week 14)

**Tasks**:
- [ ] Write architecture documentation
- [ ] Write ADRs (Architecture Decision Records)
- [ ] Write API documentation
- [ ] Write operational guide
- [ ] Write performance tuning guide
- [ ] Write integration guide for higher-level systems

**Deliverables**:
- `docs/ARCHITECTURE.md`, `docs/ADR/`, `docs/API.md`
- `docs/OPERATIONS.md`, `docs/PERFORMANCE.md`, `docs/INTEGRATION_GUIDE.md`

---

## Example Integration Patterns

### Relational Database on Nanostore

```rust
struct RelationalDatabase {
    storage: Arc<Nanostore::Database>,
}

impl RelationalDatabase {
    // SQL transaction maps 1:1 to Nanostore transaction
    fn execute_transaction(&self, sql_statements: Vec<SqlStatement>) -> Result<()> {
        // Begin ONE storage transaction for entire SQL transaction
        let storage_txn = self.storage.begin_transaction()?;
        
        for stmt in sql_statements {
            match stmt {
                SqlStatement::Insert { table, row } => {
                    self.insert_row(&storage_txn, &table, &row)?;
                }
                SqlStatement::Update { table, key, row } => {
                    self.update_row(&storage_txn, &table, &key, &row)?;
                }
                SqlStatement::Delete { table, key } => {
                    self.delete_row(&storage_txn, &table, &key)?;
                }
            }
        }
        
        // Commit entire SQL transaction atomically
        storage_txn.commit()
    }
    
    fn insert_row(&self, txn: &Transaction, table: &str, row: &Row) -> Result<()> {
        // Get table within existing transaction
        let storage_table = txn.table(table)?;
        
        // Insert row data
        let key = format!("{}:{}", table, row.id);
        storage_table.put(key.as_bytes(), &row.serialize())?;
        
        // Update indexes (all within same transaction)
        for (col, index) in &self.get_indexes(table)? {
            let index_key = format!("{}:{}:{}", table, col, row.get(col));
            let index_table = txn.table(&format!("idx_{}", index))?;
            index_table.put(index_key.as_bytes(), row.id.as_bytes())?;
        }
        
        Ok(())
    }
    
    // Example: Hash join using memory table within a transaction
    fn hash_join(&self, txn: &Transaction, left: &str, right: &str, join_col: &str) -> Result<Vec<Row>> {
        // Create temporary memory table within existing transaction
        let hash_table_config = TableConfig {
            table_type: TableType::Memory,
            engine: EngineType::BTree,
            ..Default::default()
        };
        txn.create_temp_table("join_hash", hash_table_config)?;
        let hash_table = txn.table("join_hash")?;
        
        // Build phase: scan smaller table into memory
        let left_table = txn.table(left)?;
        let cursor = left_table.scan(&[], &[])?;
        for result in cursor {
            let (key, value) = result?;
            let row = Row::deserialize(&value)?;
            let join_key = row.get(join_col).as_bytes();
            hash_table.put(join_key, &value)?;
        }
        
        // Probe phase: scan larger table and lookup
        let mut results = Vec::new();
        let right_table = txn.table(right)?;
        let cursor = right_table.scan(&[], &[])?;
        for result in cursor {
            let (_, value) = result?;
            let row = Row::deserialize(&value)?;
            let join_key = row.get(join_col).as_bytes();
            
            if let Some(matching) = hash_table.get(join_key)? {
                let left_row = Row::deserialize(&matching)?;
                results.push(Row::join(&left_row, &row));
            }
        }
        
        // Memory table automatically cleaned up on transaction end
        Ok(results)
    }
}
```

**Key Points**:
- SQL `BEGIN TRANSACTION` → `Nanostore::Database::begin_transaction()`
- All SQL operations within transaction → All Nanostore operations within same transaction
- SQL `COMMIT` → `Nanostore::Transaction::commit()`
- SQL `ROLLBACK` → `Nanostore::Transaction::rollback()`
- No nested transactions needed - clean 1:1 mapping

### Graph Database on Nanostore

```rust
struct Graph {
    vertices: Nanostore::Table,  // vertex_id -> properties
    edges: Nanostore::Table,      // src_id + edge_type + dst_id -> properties
    outgoing_index: Nanostore::Index,  // src_id + edge_type -> [dst_id]
    incoming_index: Nanostore::Index,  // dst_id + edge_type -> [src_id]
}

impl Graph {
    fn traverse(&self, start: VertexId, edge_type: &str) -> Result<Vec<VertexId>> {
        // Use prefix scan to find all outgoing edges
        let prefix = format!("{}:{}", start, edge_type);
        let cursor = self.outgoing_index.prefix_scan(prefix.as_bytes())?;
        
        cursor.map(|(_, dst_id)| VertexId::from_bytes(&dst_id)).collect()
    }
}
```

---

## Key Design Decisions

1. **Page Size: 4KB** - Matches OS page size, good balance
2. **Storage Engine: B-Tree First** - Simpler, good for read-heavy workloads
3. **Concurrency: Readers-Writer Lock** - Simple, correct, can upgrade to MVCC later
4. **Durability: WAL** - Standard approach, configurable sync
5. **Transaction Model: 1:1 Mapping** - One higher-level transaction = One Nanostore transaction
6. **Isolation: Read-Committed** - Strong guarantees, can upgrade to serializable later
7. **Key/Value Types: Byte Slices** - Maximum flexibility
8. **API Philosophy: Low-Level Primitives** - Storage layer, not end-user database

---

## Timeline Summary

| Phase | Duration | Deliverable |
|-------|----------|-------------|
| Phase 1: Foundation | 3 weeks | VFS, Pager, WAL |
| Phase 2: Storage | 3 weeks | B-Tree table |
| Phase 3: Performance | 2 weeks | Cache, Indexes |
| Phase 4: API | 2 weeks | Core API, Transactions |
| Phase 5: Interfaces (Optional) | 2 weeks | REST, CLI |
| Phase 6: Quality | 2 weeks | Testing, Docs |
| **Total** | **14 weeks** | **Production-ready storage layer** |

---

## Success Criteria

### Phase 1-2 (MVP)
- [ ] Can create/open database
- [ ] Can insert/get/delete key-value pairs
- [ ] Survives crashes without data loss
- [ ] Passes all unit tests
- [ ] Example integration with simple relational layer

### Phase 3-4 (Beta)
- [ ] Performance >10K ops/sec
- [ ] Transaction support working
- [ ] Cache improves performance
- [ ] API is ergonomic for storage layer use
- [ ] Example integrations with relational and graph layers

### Phase 6 (Production)
- [ ] Stress tests pass
- [ ] Documentation complete
- [ ] Integration guide complete
- [ ] Ready for use by higher-level database systems

---

## Dependencies

```toml
[dependencies]
async-trait = "0.1"      # Already present
sha2 = "0.10"            # Checksums
parking_lot = "0.12"     # Better locks
thiserror = "1.0"        # Error handling

[dev-dependencies]
proptest = "1"           # Property testing
criterion = "0.5"        # Benchmarking
tempfile = "3"           # Test utilities

[optional]
axum = "0.7"             # REST API (optional)
tokio = "1"              # Async runtime (optional)
clap = "4"               # CLI parsing (optional)
serde = "1"              # Serialization (optional)
```

---

## Next Steps

1. Review this plan with stakeholders and higher-level system developers
2. Set up project tracking (use bd for issue tracking)
3. Begin Phase 1.1 (VFS implementation)
4. Establish CI/CD pipeline
5. Create initial ADRs for key decisions
6. Engage with higher-level system developers to validate API design

---

**End of Implementation Plan**