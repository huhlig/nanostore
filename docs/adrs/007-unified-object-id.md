# ADR-007: Unified ObjectId System

**Status**: Superseded by [ADR-012](./012-unified-table-architecture.md)
**Date**: 2026-05-10
**Deciders**: Hans W. Uhlig, Development Team
**Technical Story**: Transaction layer improvements (Nanostore-i5l, Nanostore-6nx)

> **Note**: This ADR has been superseded by [ADR-012: Unified Table Architecture](./012-unified-table-architecture.md).
> The core concept of using `ObjectId` for all storage objects remains, but the implementation has evolved
> to remove the `TableId` and `IndexId` wrapper types entirely, using `ObjectId` directly throughout the codebase.

## Context

The transaction layer currently uses [`TableId`](../../src/table/traits.rs) for all operations, but the database also has indexes that need transactional support. The question arose: should the transaction layer work with both [`TableId`](../../src/table/traits.rs) and [`IndexId`](../../src/index/traits.rs), or should there be a unified identifier system?

**Current State:**
- Transaction operations use [`TableId`](../../src/table/traits.rs) exclusively
- Indexes have separate [`IndexId`](../../src/index/traits.rs) type
- [`IndexInfo.table_id`](../../src/index/traits.rs:113) maintains parent table relationship
- No mechanism for transaction layer to work with indexes

**Key Questions:**
1. Should transaction layer understand the difference between tables and indexes?
2. How should index maintenance be coordinated with table operations?
3. Where should the semantic distinction between tables and indexes live?

## Decision

We will implement a **unified ObjectId system** where:

1. **ObjectId as Base Type**: Create `ObjectId` as the fundamental identifier for any storage object
2. **Type-Safe Wrappers**: [`TableId`](../../src/table/traits.rs) and [`IndexId`](../../src/index/traits.rs) become type-safe wrappers around `ObjectId`
3. **Transaction Layer Uses ObjectId**: Transaction operations work with `ObjectId`, treating all storage objects uniformly
4. **Semantic Layer at Database**: The [`Database`](../../src/kvdb.rs) layer maintains the semantic distinction and handles index maintenance

**Type Hierarchy:**
```rust
// Base type - used by transaction/storage layers
pub struct ObjectId(u64);

// Type-safe wrappers - used by API layer
pub struct TableId(ObjectId);
pub struct IndexId(ObjectId);

// Conversions
impl From<TableId> for ObjectId { ... }
impl From<IndexId> for ObjectId { ... }
impl TryFrom<ObjectId> for TableId { ... }  // With catalog validation
impl TryFrom<ObjectId> for IndexId { ... }  // With catalog validation
```

## Rationale

### 1. Indexes Are "Specialty Tables" at Storage Layer

At the transaction and storage layer, indexes are semantically just key-value stores:
- Both have keys and values
- Both need MVCC version chains
- Both need conflict detection
- Both need WAL logging
- Both need page allocation

The only difference is **semantic**: indexes derive their content from tables. This semantic relationship is a higher-level concern.

### 2. Transaction Layer Doesn't Need Index-Specific Logic

The transaction layer's responsibilities are:
- Conflict detection (write-write, read-write)
- Lock management
- Write set tracking
- Commit/rollback coordination

None of these operations need to know whether an object is a table or an index. They just need to track:
- Which objects were read
- Which objects were written
- Whether conflicts exist

### 3. Index Maintenance Belongs at Database Layer

Index maintenance logic (updating indexes when tables change) is a **semantic concern** that belongs at the [`Database`](../../src/kvdb.rs) layer:

```rust
impl Database {
    pub fn insert(&mut self, table: TableId, key: &[u8], value: &[u8]) 
        -> Result<(), DatabaseError> 
    {
        let mut txn = self.begin_write()?;
        
        // 1. Insert into table
        txn.put(table.into(), key, value)?;
        
        // 2. Update all indexes on this table
        for index in self.indexes_for_table(table) {
            let index_key = index.extract_key(key, value)?;
            let index_value = index.build_value(key)?;
            txn.put(index.id().into(), &index_key, &index_value)?;
        }
        
        // 3. Commit atomically (table + all indexes)
        txn.commit()?;
        Ok(())
    }
}
```

This design:
- ✅ Keeps transaction layer simple and uniform
- ✅ Maintains atomicity (table + indexes in one transaction)
- ✅ Allows flexible index maintenance strategies
- ✅ Preserves type safety at API boundaries

### 4. Unified Write Set and Conflict Detection

With `ObjectId`, the transaction layer can use a single write set:

```rust
pub struct Transaction {
    write_set: HashMap<(ObjectId, Vec<u8>), Option<Vec<u8>>>,
    read_set: HashSet<(ObjectId, Vec<u8>)>,
    // ...
}
```

This enables:
- Single conflict detection map for all objects
- Unified lock management
- Simpler transaction implementation
- No special-casing for indexes

## Consequences

### Positive

- **Simpler Transaction Layer**: No need to handle tables and indexes differently
- **Type Safety Preserved**: API layer still has type-safe [`TableId`](../../src/table/traits.rs) and [`IndexId`](../../src/index/traits.rs)
- **Flexible Index Strategies**: Database layer can implement various index maintenance approaches
- **Unified Conflict Detection**: Single write set and conflict map for all objects
- **Clear Separation of Concerns**: Transaction = ACID, Database = semantics
- **Future-Proof**: Easy to add new object types (materialized views, etc.)

### Negative

- **Catalog Dependency**: Need catalog to validate `ObjectId` → [`TableId`](../../src/table/traits.rs)/[`IndexId`](../../src/index/traits.rs) conversions
- **Type Erasure**: Transaction layer loses type information (by design)
- **Potential Confusion**: Developers must understand the layering
- **Migration Effort**: Need to update existing transaction code

### Mitigations

1. **Catalog Integration**: Provide helper methods for safe conversions
2. **Documentation**: Clear ADR and code comments explaining the design
3. **Type Safety**: Preserve type safety at API boundaries
4. **Gradual Migration**: Can implement incrementally

## Implementation Details

### ObjectId Type

```rust
/// Unified identifier for any storage object (table, index, etc.)
#[derive(Clone, Copy, Debug, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct ObjectId(u64);

impl ObjectId {
    pub fn as_u64(&self) -> u64 {
        self.0
    }
    
    pub fn to_bytes(&self) -> [u8; 8] {
        self.0.to_le_bytes()
    }
}

impl From<u64> for ObjectId {
    fn from(value: u64) -> Self {
        Self(value)
    }
}
```

### TableId Wrapper

```rust
/// Type-safe wrapper for table identifiers
#[derive(Clone, Copy, Debug, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct TableId(ObjectId);

impl From<TableId> for ObjectId {
    fn from(id: TableId) -> Self {
        id.0
    }
}

impl TableId {
    pub fn as_object_id(&self) -> ObjectId {
        self.0
    }
}
```

### IndexId Wrapper

```rust
/// Type-safe wrapper for index identifiers
#[derive(Clone, Copy, Debug, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct IndexId(ObjectId);

impl From<IndexId> for ObjectId {
    fn from(id: IndexId) -> Self {
        id.0
    }
}

impl IndexId {
    pub fn as_object_id(&self) -> ObjectId {
        self.0
    }
}
```

### Transaction Interface

```rust
pub trait TransactionOps {
    fn get(&self, object: ObjectId, key: &[u8]) -> TransactionResult<Option<ValueBuf>>;
    fn put(&mut self, object: ObjectId, key: &[u8], value: &[u8]) -> TransactionResult<()>;
    fn delete(&mut self, object: ObjectId, key: &[u8]) -> TransactionResult<bool>;
    fn range_delete(&mut self, object: ObjectId, bounds: ScanBounds) -> TransactionResult<u64>;
    // ...
}
```

### Database Layer Index Maintenance

```rust
impl Database {
    /// Insert with automatic index maintenance
    pub fn insert(&mut self, table: TableId, key: &[u8], value: &[u8]) 
        -> Result<(), DatabaseError> 
    {
        let mut txn = self.begin_write(Durability::SyncOnCommit)?;
        
        // Insert into table
        txn.put(table.into(), key, value)?;
        
        // Update indexes
        let indexes = self.index_catalog.read().unwrap();
        for (_, index_info) in indexes.iter() {
            if index_info.table_id == table {
                // Extract index key from table key/value
                let index_key = self.extract_index_key(index_info, key, value)?;
                // Index value is typically the table key (for lookups)
                txn.put(index_info.id.into(), &index_key, key)?;
            }
        }
        
        txn.commit()?;
        Ok(())
    }
    
    /// Delete with automatic index maintenance
    pub fn delete(&mut self, table: TableId, key: &[u8]) 
        -> Result<bool, DatabaseError> 
    {
        let mut txn = self.begin_write(Durability::SyncOnCommit)?;
        
        // Get current value (needed for index key extraction)
        let value = txn.get(table.into(), key)?;
        
        // Delete from table
        let deleted = txn.delete(table.into(), key)?;
        
        if deleted {
            if let Some(value) = value {
                // Delete from indexes
                let indexes = self.index_catalog.read().unwrap();
                for (_, index_info) in indexes.iter() {
                    if index_info.table_id == table {
                        let index_key = self.extract_index_key(index_info, key, value.as_ref())?;
                        txn.delete(index_info.id.into(), &index_key)?;
                    }
                }
            }
        }
        
        txn.commit()?;
        Ok(deleted)
    }
}
```

## Catalog Integration

The catalog maintains the mapping between `ObjectId` and object types:

```rust
pub struct Database {
    // Unified object catalog
    object_catalog: Arc<RwLock<HashMap<ObjectId, ObjectInfo>>>,
    
    // Type-specific views
    table_catalog: Arc<RwLock<HashMap<String, TableInfo>>>,
    index_catalog: Arc<RwLock<HashMap<IndexId, IndexInfo>>>,
}

pub enum ObjectInfo {
    Table(TableInfo),
    Index(IndexInfo),
    // Future: MaterializedView, Sequence, etc.
}

impl Database {
    /// Validate and convert ObjectId to TableId
    pub fn as_table_id(&self, object: ObjectId) -> Result<TableId, DatabaseError> {
        let catalog = self.object_catalog.read().unwrap();
        match catalog.get(&object) {
            Some(ObjectInfo::Table(_)) => Ok(TableId(object)),
            Some(_) => Err(DatabaseError::NotATable(object)),
            None => Err(DatabaseError::ObjectNotFound(object)),
        }
    }
    
    /// Validate and convert ObjectId to IndexId
    pub fn as_index_id(&self, object: ObjectId) -> Result<IndexId, DatabaseError> {
        let catalog = self.object_catalog.read().unwrap();
        match catalog.get(&object) {
            Some(ObjectInfo::Index(_)) => Ok(IndexId(object)),
            Some(_) => Err(DatabaseError::NotAnIndex(object)),
            None => Err(DatabaseError::ObjectNotFound(object)),
        }
    }
}
```

## ID Allocation Strategy

**Unified ID Space:**
- All objects (tables, indexes, etc.) share a single ID space
- IDs are allocated sequentially from a single counter
- No risk of ID collision between tables and indexes

**Benefits:**
- Simple allocation logic
- No need for ID namespacing
- Easy to add new object types
- Catalog lookup is straightforward

**Example:**
```
ObjectId(1) → Table "users"
ObjectId(2) → Table "posts"
ObjectId(3) → Index "users_email_idx"
ObjectId(4) → Index "posts_author_idx"
ObjectId(5) → Table "comments"
```

## Alternatives Considered

### Alternative 1: Separate TableId and IndexId in Transaction Layer

**Approach**: Transaction layer has separate methods for tables and indexes.

```rust
pub trait TransactionOps {
    fn get_table(&self, table: TableId, key: &[u8]) -> Result<Option<ValueBuf>>;
    fn get_index(&self, index: IndexId, key: &[u8]) -> Result<Option<ValueBuf>>;
    fn put_table(&mut self, table: TableId, key: &[u8], value: &[u8]) -> Result<()>;
    fn put_index(&mut self, index: IndexId, key: &[u8], value: &[u8]) -> Result<()>;
    // ...
}
```

**Pros**:
- Type safety at transaction layer
- Explicit about object types
- No type erasure

**Cons**:
- Duplicate code for tables and indexes
- More complex transaction implementation
- Harder to add new object types
- Separate write sets and conflict maps
- Transaction layer needs to understand semantics

**Rejected because**: Adds unnecessary complexity to transaction layer without meaningful benefit.

### Alternative 2: Enum-Based ObjectId

**Approach**: `ObjectId` is an enum containing either [`TableId`](../../src/table/traits.rs) or [`IndexId`](../../src/index/traits.rs).

```rust
pub enum ObjectId {
    Table(TableId),
    Index(IndexId),
}
```

**Pros**:
- Type information preserved
- Pattern matching available
- No catalog lookup needed

**Cons**:
- Larger memory footprint (discriminant + value)
- Pattern matching required everywhere
- Harder to add new types (breaks existing code)
- Still need separate [`TableId`](../../src/table/traits.rs)/[`IndexId`](../../src/index/traits.rs) types

**Rejected because**: Defeats the purpose of unified identifier. Transaction layer shouldn't care about object type.

### Alternative 3: Namespaced IDs

**Approach**: Use high bits for object type, low bits for ID.

```rust
pub struct ObjectId(u64);

impl ObjectId {
    fn object_type(&self) -> ObjectType {
        match self.0 >> 56 {
            0 => ObjectType::Table,
            1 => ObjectType::Index,
            _ => ObjectType::Unknown,
        }
    }
    
    fn local_id(&self) -> u64 {
        self.0 & 0x00FFFFFFFFFFFFFF
    }
}
```

**Pros**:
- Type information in ID itself
- No catalog lookup for type
- Efficient bit operations

**Cons**:
- Reduces ID space (56 bits instead of 64)
- Couples ID format to object types
- Harder to add new types (limited namespace)
- Type information leaks into storage layer

**Rejected because**: Premature optimization. Catalog lookup is cheap and provides more flexibility.

## Performance Considerations

### ID Conversion Overhead

- **From [`TableId`](../../src/table/traits.rs)/[`IndexId`](../../src/index/traits.rs) to `ObjectId`**: Zero-cost (newtype unwrapping)
- **From `ObjectId` to [`TableId`](../../src/table/traits.rs)/[`IndexId`](../../src/index/traits.rs)**: Requires catalog lookup (rare operation)

**Optimization**: Cache object type information in hot paths if needed.

### Write Set Size

Unified write set has same memory footprint as separate sets:
- Before: `HashMap<(TableId, Key), Value>` + `HashMap<(IndexId, Key), Value>`
- After: `HashMap<(ObjectId, Key), Value>`

No performance difference.

### Conflict Detection

Unified conflict detection is **simpler and faster**:
- Single hash map lookup instead of two
- No need to check both table and index conflict maps
- Fewer branches in hot path

## Migration Path

**Phase 1**: Add `ObjectId` type (this ADR)
- Define `ObjectId` in [`src/types.rs`](../../src/types.rs)
- Update [`TableId`](../../src/table/traits.rs) and [`IndexId`](../../src/index/traits.rs) to wrap `ObjectId`
- Add conversion methods

**Phase 2**: Update transaction layer (Nanostore-6nx)
- Change [`TransactionOps`](../../src/txn/transaction.rs:78) to use `ObjectId`
- Update write_set and read_set to use `ObjectId`
- Update conflict detection to use `ObjectId`

**Phase 3**: Implement index maintenance (Nanostore-j89)
- Add index maintenance logic to [`Database`](../../src/kvdb.rs)
- Implement `insert()`, `update()`, `delete()` with automatic index updates
- Add index key extraction logic

**Phase 4**: Testing and validation
- Test table operations with indexes
- Test conflict detection across tables and indexes
- Benchmark performance

## Monitoring and Metrics

Track these metrics:
- Object allocation rate (tables vs indexes)
- Transaction write set size (objects touched)
- Conflict rate by object type
- Index maintenance overhead
- Catalog lookup performance

## Testing Strategy

1. **Unit Tests**: `ObjectId` conversions and type safety
2. **Integration Tests**: Transactions with mixed table/index operations
3. **Conflict Tests**: Write-write conflicts across tables and indexes
4. **Index Tests**: Automatic index maintenance during table operations
5. **Performance Tests**: Benchmark unified vs separate ID systems

## References

- [Transaction Layer Improvements](../TRANSACTION_LAYER_IMPROVEMENTS.md)
- [Transaction Implementation](../../src/txn/transaction.rs)
- [Database Implementation](../../src/kvdb.rs)
- [Table Traits](../../src/table/traits.rs)
- [Index Traits](../../src/index/traits.rs)

## Related ADRs

- [ADR-003: MVCC Concurrency](./003-mvcc-concurrency.md)
- [ADR-004: Multiple Storage Engines](./004-multiple-storage-engines.md)
- [ADR-006: Sharded Concurrency](./006-sharded-concurrency.md)

## Related Issues

- **Nanostore-i5l**: Transaction layer improvements (completed, led to this issue)
- **Nanostore-6nx**: Implement unified ObjectId system (this ADR)
- **Nanostore-j89**: Implement index maintenance logic (depends on this ADR)

---

**Last Updated**: 2026-05-10