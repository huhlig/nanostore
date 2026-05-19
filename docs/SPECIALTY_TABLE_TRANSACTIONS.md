# Specialty Table Transaction Integration

This document describes the transaction integration pattern for specialty tables in Nanostore. Specialty tables are table engines that provide domain-specific query capabilities beyond standard key-value operations, including:

- **ApproximateMembership** (Bloom filters) - Probabilistic membership testing
- **FullTextSearch** - Full-text indexing and search
- **VectorSearch** - Vector similarity search (HNSW)
- **GeoSpatial** - Geospatial indexing and queries (R-Tree)
- **TimeSeries** - Time series data management
- **GraphAdjacency** - Graph edge traversal

## Transaction API Overview

### Table Context Management

The transaction layer uses a **table context** pattern to route specialty table operations to the correct table engine. The context is set via `with_table(table_id)` and cleared via `clear_table_context()`.

```rust
// Set table context for specialty operations
txn.with_table(bloom_table_id);
// ... perform specialty operations ...
txn.clear_table_context();
```

### Scoped Helper Methods

For convenience, most specialty table types provide scoped helper methods that automatically manage the table context:

```rust
// Bloom filters (dyn-compatible)
txn.with_bloom(table_id, |bloom| {
    bloom.insert_key(b"key")?;
    bloom.might_contain(b"key")
})?;

// Vector search (dyn-compatible)
txn.with_vector(table_id, |vector| {
    vector.insert_vector(b"id", &[1.0, 2.0, 3.0])?;
    vector.search_vector(&[1.0, 2.0, 3.0], options)
})?;

// Geospatial (dyn-compatible)
txn.with_geospatial(table_id, |geo| {
    geo.insert_geometry(b"id", geometry)?;
    geo.intersects(query, limit)
})?;

// Full-text search (dyn-compatible)
txn.with_fulltext(table_id, |fulltext| {
    fulltext.index_document(b"doc1", &fields)?;
    fulltext.search(query, limit)
})?;
```

**Note:** `GraphAdjacency` and `TimeSeries` use Generic Associated Types (GATs) which are not dyn-compatible. Use `with_table()` directly:

```rust
txn.with_table(graph_table_id);
GraphAdjacency::add_edge(&mut txn, source, label, target, edge_id)?;
txn.clear_table_context();
```

## Usage Examples

### Bloom Filters (ApproximateMembership)

```rust
use Nanostore::table::{ApproximateMembership, TableEngineKind, TableOptions};
use Nanostore::types::Durability;

// Create a bloom filter table
let bloom_id = db.create_table("bloom", TableOptions {
    engine: TableEngineKind::Bloom,
    ..Default::default()
})?;

// Insert keys transactionally
let mut txn = db.begin_write(Durability::WalOnly)?;
txn.with_bloom(bloom_id, |bloom| {
    bloom.insert_key(b"user:123")?;
    bloom.insert_key(b"user:456")?;
    Ok(())
})?;
txn.commit()?;

// Query in a read transaction
let mut read_txn = db.begin_read()?;
let exists = read_txn.with_bloom(bloom_id, |bloom| {
    bloom.might_contain(b"user:123")
})?;
assert!(exists);
```

### Full-Text Search

```rust
use Nanostore::table::{FullTextSearch, TextField, TextQuery};

let fulltext_id = db.create_table("search", TableOptions {
    engine: TableEngineKind::FullText,
    ..Default::default()
})?;

// Index documents
let mut txn = db.begin_write(Durability::WalOnly)?;
txn.with_fulltext(fulltext_id, |fulltext| {
    let fields = vec![
        TextField { name: "title", text: "Rust Programming", boost: 1.0 },
        TextField { name: "body", text: "Learning Rust is fun", boost: 0.5 },
    ];
    fulltext.index_document(b"doc1", &fields)?;
    Ok(())
})?;
txn.commit()?;

// Search
let mut read_txn = db.begin_read()?;
let results = read_txn.with_fulltext(fulltext_id, |fulltext| {
    fulltext.search(TextQuery {
        query: "Rust",
        default_field: None,
        require_positions: false,
    }, 10)
})?;
```

### Vector Search (HNSW)

```rust
use Nanostore::table::{VectorSearch, VectorSearchOptions, VectorMetric};

let vector_id = db.create_table("embeddings", TableOptions {
    engine: TableEngineKind::Hnsw,
    ..Default::default()
})?;

// Insert vectors
let mut txn = db.begin_write(Durability::WalOnly)?;
txn.with_vector(vector_id, |vector| {
    vector.insert_vector(b"item1", &[0.1, 0.2, 0.3])?;
    vector.insert_vector(b"item2", &[0.4, 0.5, 0.6])?;
    Ok(())
})?;
txn.commit()?;

// Search
let mut read_txn = db.begin_read()?;
let results = read_txn.with_vector(vector_id, |vector| {
    vector.search_vector(&[0.15, 0.25, 0.35], VectorSearchOptions {
        limit: 5,
        ef_search: Some(50),
        probes: None,
        filter: None,
    })
})?;
```

### Geospatial (R-Tree)

```rust
use Nanostore::table::{GeoSpatial, GeoPoint, GeometryRef};

let geo_id = db.create_table("locations", TableOptions {
    engine: TableEngineKind::RTree,
    ..Default::default()
})?;

// Insert geometries
let mut txn = db.begin_write(Durability::WalOnly)?;
txn.with_geospatial(geo_id, |geo| {
    let point = GeometryRef::Point(GeoPoint { x: -73.9857, y: 40.7484 });
    geo.insert_geometry(b"empire_state", point)?;
    Ok(())
})?;
txn.commit()?;

// Query
let mut read_txn = db.begin_read()?;
let hits = read_txn.with_geospatial(geo_id, |geo| {
    geo.nearest(GeoPoint { x: -73.9857, y: 40.7484 }, 5)
})?;
```

### Time Series

```rust
use Nanostore::table::TimeSeries;

let ts_id = db.create_table("metrics", TableOptions {
    engine: TableEngineKind::TimeSeries,
    ..Default::default()
})?;

// Insert time series points
let mut txn = db.begin_write(Durability::WalOnly)?;
txn.with_table(ts_id);
TimeSeries::insert_point(&mut txn, b"cpu.usage", 1700000000, 75.5)?;
TimeSeries::insert_point(&mut txn, b"cpu.usage", 1700000060, 80.2)?;
txn.clear_table_context();
txn.commit()?;

// Query range
let mut read_txn = db.begin_read()?;
read_txn.with_table(ts_id);
let cursor = TimeSeries::query_range(&read_txn, b"cpu.usage", 1700000000, 1700000100)?;
read_txn.clear_table_context();
```

### Graph Adjacency

```rust
use Nanostore::table::GraphAdjacency;

let graph_id = db.create_table("social", TableOptions {
    engine: TableEngineKind::Graph,
    ..Default::default()
})?;

// Add edges
let mut txn = db.begin_write(Durability::WalOnly)?;
txn.with_table(graph_id);
GraphAdjacency::add_edge(&mut txn, b"alice", b"follows", b"bob", b"edge1")?;
GraphAdjacency::add_edge(&mut txn, b"bob", b"follows", b"charlie", b"edge2")?;
txn.clear_table_context();
txn.commit()?;

// Traverse
let mut read_txn = db.begin_read()?;
read_txn.with_table(graph_id);
let cursor = GraphAdjacency::outgoing(&read_txn, b"alice", b"follows")?;
read_txn.clear_table_context();
```

## Transaction Semantics

### Durability

All specialty table operations are logged to the WAL (Write-Ahead Log) for durability. The durability policy controls how commits are persisted:

| Policy | Behavior |
|--------|----------|
| `MemoryOnly` | No WAL writes (in-memory tables only) |
| `WalOnly` | Write to WAL buffer, no force sync |
| `FlushOnCommit` | Flush WAL to OS, no disk sync |
| `SyncOnCommit` | Force sync to stable storage |

### Isolation

Specialty table operations follow the transaction's isolation level:

- **ReadCommitted**: Reads see only committed data
- **SnapshotIsolation**: Reads see snapshot at transaction start
- **Serializable**: Full isolation with read-write conflict detection

### Atomicity

All specialty table operations within a transaction are atomic:

- Operations are tracked in type-specific write sets (`bloom_write_set`, `fulltext_write_set`, `vector_write_set`, etc.)
- On commit, operations are applied to the underlying table engine
- On rollback, write sets are discarded without applying changes

### Consistency

Specialty table operations maintain consistency through:

1. **WAL logging**: All operations are logged before being applied
2. **Write set tracking**: Uncommitted changes are visible to the owning transaction
3. **Conflict detection**: Write-write conflicts are detected and prevented

## Write Set Types

| Specialty Table | Write Set Type | WAL Op Types |
|-----------------|----------------|--------------|
| ApproximateMembership | `HashSet<(TableId, Vec<u8>)>` | `BloomInsert` |
| FullTextSearch | `Vec<(TableId, FullTextOp)>` | `FullTextIndex`, `FullTextUpdate`, `FullTextDelete` |
| VectorSearch | `RwLock<Vec<(TableId, VectorOp)>>` | `VectorInsert`, `VectorDelete` |
| GeoSpatial | `Vec<(TableId, GeoSpatialOp)>` | `GeoInsert`, `GeoDelete` |
| TimeSeries | `RefCell<Vec<(TableId, TimeSeriesOp)>>` | `TimeSeriesInsert`, `TimeSeriesDelete` |
| GraphAdjacency | `Vec<(TableId, GraphEdgeOp)>` | `GraphAddEdge`, `GraphRemoveEdge` |

## Limitations and Best Practices

### Current Limitations

1. **Interior Mutability Required**: Some specialty tables (PagedFullTextIndex, PagedRTree) require interior mutability (`RwLock`) for their mutable state before commit-time application can be fully enabled. Operations are logged to WAL but actual application is deferred.

2. **Write-Set Visibility**: For some specialty tables, uncommitted writes may not be visible to search/query operations within the same transaction. This is being addressed as interior mutability is added.

3. **GAT Limitations**: `GraphAdjacency` and `TimeSeries` use Generic Associated Types which are not dyn-compatible, requiring direct trait method calls instead of scoped helpers.

### Best Practices

1. **Use Scoped Helpers**: Prefer `with_bloom()`, `with_vector()`, etc. for automatic context management.

2. **Batch Operations**: Group multiple specialty operations within a single transaction for atomicity.

3. **Appropriate Durability**: Use `WalOnly` for performance-critical paths, `SyncOnCommit` for critical data.

4. **Table Context**: Always set table context before specialty operations and clear afterward (or use scoped helpers).

5. **Error Handling**: Check for errors on each specialty operation; failed operations still consume WAL space.

## Migration Guide

### From Direct Table Access to Transactional Access

**Before (direct access):**
```rust
let table = db.get_table("bloom")?;
table.insert_key(b"key")?;
```

**After (transactional):**
```rust
let bloom_id = db.get_table_id("bloom")?;
let mut txn = db.begin_write(Durability::WalOnly)?;
txn.with_bloom(bloom_id, |bloom| {
    bloom.insert_key(b"key")?;
    Ok(())
})?;
txn.commit()?;
```

### Key Changes

1. **Table ID Required**: Specialty operations require the table ID, not the table reference.
2. **Transaction Scope**: All operations must occur within a transaction.
3. **Context Management**: Use `with_table()` or scoped helpers to set the table context.
4. **Commit/Rollback**: Changes are not permanent until `commit()` is called.

## Architecture

### Trait-Based Design

The transaction layer implements specialty table traits directly on `Transaction<FS>`:

```
Transaction<FS>
├── impl ApproximateMembership
├── impl FullTextSearch
├── impl VectorSearch
├── impl GeoSpatial
├── impl TimeSeries
└── impl GraphAdjacency
```

Each implementation:
1. Checks transaction state (must be `Active`)
2. Validates table context is set
3. Logs operation to WAL with type-specific `WriteOpType`
4. Records operation in type-specific write set
5. Delegates to underlying engine for read/query operations

### Commit Flow

```
commit()
├── Write COMMIT record to WAL
├── Apply main write_set to storage engines
├── Apply bloom_write_set (if applicable)
├── Apply graph_write_set (if applicable)
├── Apply timeseries_write_set (if applicable)
├── Apply vector_write_set (if applicable)
├── Apply geospatial_write_set (if applicable)
├── Apply fulltext_write_set (if applicable)
├── Update current LSN
├── Transition to Committed state
└── Release all locks
```

### Rollback Flow

Rollback is implicit - the `Transaction` struct is dropped without applying write sets. The WAL contains the operations but they are never applied to storage engines.
