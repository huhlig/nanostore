# ADR-012: Unified Table Architecture

**Status**: Accepted  
**Date**: 2026-05-11  
**Deciders**: Hans W. Uhlig, Development Team  
**Technical Story**: Phase 7 - Unified table/index architecture (Nanostore-y33)  
**Supersedes**: [ADR-007](./007-unified-object-id.md), [ADR-011](./011-indexes-as-specialty-tables.md)

## Context

Nanostore initially had separate type systems for tables and indexes, with distinct traits, identifiers, and APIs. This created several problems:

1. **Code Duplication**: Similar functionality implemented separately for tables and indexes
2. **Complex Transaction Layer**: Had to handle tables and indexes differently despite identical storage semantics
3. **Rigid Architecture**: Adding new index types required changes across multiple layers
4. **Inconsistent APIs**: Different error types, capabilities, and patterns for tables vs indexes
5. **Maintenance Burden**: Changes had to be replicated across parallel hierarchies

**Previous State:**
- Separate `TableId` and `IndexId` types
- Distinct `Table` and `Index` trait hierarchies
- Different error types (`TableError` vs `IndexError`)
- Parallel capability systems
- Index-specific transaction operations

**Key Insight:**
At the storage and transaction layer, indexes are semantically just specialized key-value stores. The only difference is their **purpose** (derived from parent tables), not their **implementation** (MVCC, WAL, conflict detection, etc.).

## Decision

We unify tables and indexes under a **single Table abstraction** with:

1. **Unified ObjectId**: All storage objects use `ObjectId` (no `TableId`/`IndexId` wrappers)
2. **Single Trait Hierarchy**: One `Table` trait with capability-based specialization
3. **TableKind Discriminator**: `TableKind::Regular` vs `TableKind::Index` distinguishes purpose
4. **Capability Traits**: Specialty tables implement additional traits (e.g., `DenseOrdered`, `VectorSearch`)
5. **Unified Error Types**: Single `TableResult<T>` and `TableError` for all operations
6. **Database Layer Semantics**: Index maintenance logic lives at the Database layer, not transaction layer

### Type System

```rust
// Unified identifier - no wrappers
pub struct ObjectId(u64);

// Table kind distinguishes purpose
pub enum TableKind {
    Regular,
    Index {
        parent_table: ObjectId,
        index_kind: IndexKind,
    },
}

// Base table trait - all storage objects implement this
pub trait Table {
    type Reader<'a>: TableReader where Self: 'a;
    type Writer<'a>: TableWriter where Self: 'a;
    
    fn table_id(&self) -> ObjectId;
    fn name(&self) -> &str;
    fn kind(&self) -> TableEngineKind;
    fn capabilities(&self) -> TableCapabilities;
    fn reader(&self, snapshot_lsn: LogSequenceNumber) -> TableResult<Self::Reader<'_>>;
    fn writer(&self, tx_id: TransactionId, snapshot_lsn: LogSequenceNumber) -> TableResult<Self::Writer<'_>>;
    fn stats(&self) -> TableResult<TableStatistics>;
}

// Specialty table traits for index-specific operations
pub trait DenseOrdered { /* B-Tree secondary indexes */ }
pub trait SparseOrdered { /* LSM SSTable range indexes */ }
pub trait ApproximateMembership { /* Bloom filters */ }
pub trait FullTextSearch { /* Text search */ }
pub trait VectorSearch { /* Vector similarity */ }
pub trait GraphAdjacency { /* Graph traversal */ }
pub trait TimeSeries { /* Time-series queries */ }
pub trait GeoSpatial { /* Spatial queries */ }
```

## Rationale

### 1. Storage Layer Uniformity

At the storage layer, all objects need:
- **MVCC version chains**: Track multiple versions per key
- **WAL logging**: Durability for all mutations
- **Page allocation**: Manage disk/memory pages
- **Conflict detection**: Prevent write-write conflicts
- **Lock management**: Coordinate concurrent access

These are **implementation concerns**, not semantic ones. The transaction and storage layers don't need to know whether an object is a "table" or an "index" - they just need to manage key-value pairs with ACID properties.

### 2. Capability-Based Design

Instead of separate type hierarchies, we use **capability traits**:

```rust
// Regular B-Tree table
impl Table for BTreeTable { /* base operations */ }
impl PointLookup for BTreeTable { /* get, contains */ }
impl OrderedScan for BTreeTable { /* scan */ }
impl MutableTable for BTreeTable { /* put, delete */ }

// B-Tree secondary index (same implementation, different purpose)
impl Table for BTreeTable { /* base operations */ }
impl PointLookup for BTreeTable { /* get, contains */ }
impl OrderedScan for BTreeTable { /* scan */ }
impl MutableTable for BTreeTable { /* put, delete */ }
impl DenseOrdered for BTreeTable { /* insert_entry, delete_entry */ }
```

The **same implementation** can serve as both a regular table and an index. The `TableKind` field in metadata distinguishes their purpose.

### 3. Simplified Transaction Layer

Transaction operations use `ObjectId` uniformly:

```rust
pub trait TransactionOps {
    fn get(&self, object: ObjectId, key: &[u8]) -> TransactionResult<Option<ValueBuf>>;
    fn put(&mut self, object: ObjectId, key: &[u8], value: &[u8]) -> TransactionResult<()>;
    fn delete(&mut self, object: ObjectId, key: &[u8]) -> TransactionResult<bool>;
    // No separate index_get, index_put, etc.
}
```

Benefits:
- Single write set: `HashMap<(ObjectId, Vec<u8>), Option<Vec<u8>>>`
- Unified conflict detection
- Simpler lock management
- No special-casing for indexes

### 4. Database Layer Handles Semantics

Index maintenance is a **semantic concern** that belongs at the Database layer:

```rust
impl Database {
    pub fn insert(&mut self, table: ObjectId, key: &[u8], value: &[u8]) -> Result<()> {
        let mut txn = self.begin_write()?;
        
        // Insert into table
        txn.put(table, key, value)?;
        
        // Update indexes (explicit, coordinated by Database layer)
        for index_id in self.indexes_for_table(table) {
            let index_info = self.get_table_info(index_id)?;
            if let TableKind::Index { index_kind, .. } = index_info.options.kind {
                let index_key = self.extract_index_key(&index_info, key, value)?;
                txn.put(index_id, &index_key, key)?;
            }
        }
        
        txn.commit()?;
        Ok(())
    }
}
```

This design:
- ✅ Keeps transaction layer simple and uniform
- ✅ Maintains atomicity (table + indexes in one transaction)
- ✅ Allows flexible index maintenance strategies (sync, deferred, async)
- ✅ Enables index-specific logic without polluting lower layers

### 5. Extensibility

Adding new specialty table types is straightforward:

1. Define a new capability trait (e.g., `trait SpatialIndex`)
2. Implement it on existing table engines
3. Add to `IndexKind` enum
4. No changes to transaction or storage layers

## Implementation Details

### TableInfo Structure

```rust
pub struct TableInfo {
    pub id: ObjectId,
    pub name: String,
    pub options: TableOptions,
    pub root: Option<PhysicalLocation>,
    pub created_lsn: LogSequenceNumber,
    pub stale: bool,  // Only relevant for indexes
}

pub struct TableOptions {
    pub engine: TableEngineKind,
    pub key_encoding: KeyEncoding,
    pub compression: Option<CompressionKind>,
    pub encryption: Option<EncryptionKind>,
    pub page_size: Option<usize>,
    pub format_version: u32,
    pub kind: TableKind,  // Regular or Index
    pub index_fields: Vec<IndexField>,  // Only for indexes
    pub unique: bool,  // Only for indexes
    pub consistency: Option<IndexConsistency>,  // Only for indexes
}
```

### Catalog Organization

```rust
pub struct Database {
    // Unified catalog - all objects
    object_catalog: Arc<RwLock<HashMap<ObjectId, TableInfo>>>,
    
    // Fast lookups by name
    name_to_id: Arc<RwLock<HashMap<String, ObjectId>>>,
    
    // Index relationships
    table_indexes: Arc<RwLock<HashMap<ObjectId, Vec<ObjectId>>>>,
}
```

### Migration from Old System

**Phase 1: Type System** (Completed)
- Remove `TableId` and `IndexId` type aliases
- Use `ObjectId` directly throughout codebase
- Update all function signatures

**Phase 2: Trait Unification** (Completed)
- Move index traits to `src/table/traits.rs`
- Rename traits (e.g., `DenseOrderedIndex` → `DenseOrdered`)
- Unify error types (`IndexError` → `TableError`)
- Deprecate old `Index` trait hierarchy

**Phase 3: Implementation Updates** (In Progress)
- Update existing implementations to use unified traits
- Implement specialty traits on table engines
- Update tests to use new APIs

**Phase 4: Database Layer** (Planned)
- Implement index maintenance in Database layer
- Add index extraction logic
- Support different consistency models

## Consequences

### Positive

1. **Simplified Architecture**: Single trait hierarchy instead of parallel ones
2. **Code Reuse**: Same implementations serve multiple purposes
3. **Easier Maintenance**: Changes in one place, not duplicated
4. **Better Extensibility**: New specialty types don't require layer changes
5. **Cleaner Transaction Layer**: No index-specific logic
6. **Unified Error Handling**: Single error type across all operations
7. **Flexible Index Strategies**: Database layer can implement various maintenance approaches
8. **Future-Proof**: Easy to add materialized views, sequences, etc.

### Negative

1. **Conceptual Shift**: Developers must understand "indexes are specialty tables"
2. **Catalog Complexity**: Need to track table-index relationships
3. **Type Safety Trade-off**: Lost compile-time distinction between tables and indexes
4. **Migration Effort**: Significant refactoring of existing code
5. **Documentation Burden**: Must clearly explain the unified model

### Mitigations

1. **Comprehensive Documentation**: This ADR, code comments, examples
2. **Helper Methods**: Database layer provides convenience methods for common patterns
3. **Runtime Validation**: Catalog enforces table-index relationships
4. **Gradual Migration**: Can implement incrementally without breaking existing code
5. **Type Markers**: `TableKind` enum provides runtime type information

## Usage Examples

### Creating a Regular Table

```rust
let options = TableOptions {
    engine: TableEngineKind::BTree,
    key_encoding: KeyEncoding::Raw,
    compression: None,
    encryption: None,
    page_size: Some(4096),
    format_version: 1,
    kind: TableKind::Regular,
    index_fields: vec![],
    unique: false,
    consistency: None,
};

let table_id = db.create_table("users", options)?;
```

### Creating a Secondary Index

```rust
let options = TableOptions {
    engine: TableEngineKind::BTree,
    key_encoding: KeyEncoding::Raw,
    compression: None,
    encryption: None,
    page_size: Some(4096),
    format_version: 1,
    kind: TableKind::Index {
        parent_table: users_table_id,
        index_kind: IndexKind::DenseOrdered,
    },
    index_fields: vec![
        IndexField {
            name: "email".to_string(),
            encoding: KeyEncoding::Raw,
            descending: false,
        }
    ],
    unique: true,
    consistency: Some(IndexConsistency::Synchronous),
};

let index_id = db.create_table("users_email_idx", options)?;
```

### Using Specialty Table Traits

```rust
// Get table as specialty type
let index = db.get_table(index_id)?;

// Check if it implements DenseOrdered
if let Some(dense_index) = index.as_any().downcast_ref::<dyn DenseOrdered>() {
    // Use index-specific operations
    dense_index.insert_entry(index_key, primary_key)?;
    let cursor = dense_index.scan(bounds)?;
}
```

### Transaction Operations (Unified)

```rust
let mut txn = db.begin_write()?;

// Same API for tables and indexes
txn.put(users_table_id, user_key, user_value)?;
txn.put(email_index_id, email_key, user_key)?;

// Single conflict detection for all objects
txn.commit()?;
```

## Performance Considerations

### Memory Overhead

- **Before**: Separate write sets for tables and indexes
- **After**: Single unified write set
- **Impact**: Slightly reduced memory usage, simpler allocation

### Conflict Detection

- **Before**: Check both table and index conflict maps
- **After**: Single hash map lookup
- **Impact**: Faster conflict detection, fewer branches

### Catalog Lookups

- **Before**: Separate table and index catalogs
- **After**: Unified catalog with type field
- **Impact**: Negligible (catalog lookups are rare in hot paths)

### Code Size

- **Before**: Duplicate implementations for tables and indexes
- **After**: Shared implementations with capability traits
- **Impact**: Reduced binary size, better instruction cache utilization

## Testing Strategy

1. **Unit Tests**: Verify trait implementations on table engines
2. **Integration Tests**: Test table and index operations together
3. **Conflict Tests**: Verify unified conflict detection
4. **Catalog Tests**: Test table-index relationship tracking
5. **Migration Tests**: Ensure backward compatibility during transition
6. **Performance Tests**: Benchmark unified vs separate systems

## Monitoring and Metrics

Track these metrics:
- Object allocation rate (regular tables vs indexes)
- Transaction write set size (objects touched)
- Conflict rate by object type
- Index maintenance overhead
- Catalog lookup performance
- Memory usage (unified vs separate write sets)

## Alternatives Considered

### Alternative 1: Keep Separate Type Hierarchies

**Approach**: Maintain distinct `Table` and `Index` traits with separate identifiers.

**Pros**:
- Compile-time type safety
- Explicit about object types
- No conceptual shift required

**Cons**:
- Code duplication
- Complex transaction layer
- Harder to extend
- Maintenance burden

**Rejected because**: Unnecessary complexity without meaningful benefit. Storage layer doesn't need semantic distinctions.

### Alternative 2: Enum-Based Dispatch

**Approach**: Use `enum TableOrIndex { Table(Table), Index(Index) }` for unified handling.

**Pros**:
- Type information preserved
- Pattern matching available

**Cons**:
- Larger memory footprint
- Pattern matching required everywhere
- Still need separate trait hierarchies
- Harder to add new types

**Rejected because**: Defeats the purpose of unification. Still maintains parallel hierarchies.

### Alternative 3: Marker Traits Only

**Approach**: Use marker traits like `trait IsTable` and `trait IsIndex` without capability traits.

**Pros**:
- Simple type system
- Clear type markers

**Cons**:
- No capability-based specialization
- Can't express index-specific operations
- Loses flexibility

**Rejected because**: Doesn't provide enough expressiveness for specialty table operations.

## Related ADRs

- **[ADR-007: Unified ObjectId System](./007-unified-object-id.md)** - Superseded by this ADR (ObjectId concept retained, but wrappers removed)
- **[ADR-011: Indexes as Specialty Tables](./011-indexes-as-specialty-tables.md)** - Superseded by this ADR (concept retained, implementation unified)
- **[ADR-003: MVCC Concurrency](./003-mvcc-concurrency.md)** - Applies uniformly to all tables
- **[ADR-004: Multiple Storage Engines](./004-multiple-storage-engines.md)** - Engines can implement specialty traits
- **[ADR-006: Sharded Concurrency](./006-sharded-concurrency.md)** - Sharding applies to all objects uniformly

## Related Issues

- **Nanostore-y33**: Phase 7 - Write ADR documenting unified table architecture (this ADR)
- **Nanostore-4ha**: Index trait implementations are completely missing (addressed by this design)
- **Nanostore-6nx**: Implement unified ObjectId system (completed, documented here)
- **Nanostore-j89**: Implement index maintenance logic (depends on this ADR)

## References

- [Table Traits Implementation](../../src/table/traits.rs)
- [Index Traits (Deprecated)](../../src/index/traits.rs)
- [Database Implementation](../../src/engine.rs)
- [Transaction Implementation](../../src/txn/transaction.rs)
- [Architecture Overview](../ARCHITECTURE.md)

## Future Work

1. **Materialized Views**: Extend specialty table concept to materialized views
2. **Sequences**: Add sequence generators as specialty tables
3. **Triggers**: Implement trigger system using specialty tables
4. **Query Optimizer**: Build query planner that leverages specialty table capabilities
5. **Index Advisor**: Analyze query patterns and recommend indexes

---

**Last Updated**: 2026-05-11  
**Authors**: Hans W. Uhlig