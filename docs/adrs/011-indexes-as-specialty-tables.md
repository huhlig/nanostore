# ADR 011: Indexes as Specialty Tables

## Status

Superseded by [ADR-012: Unified Table Architecture](./012-unified-table-architecture.md)

> **Note**: This ADR has been superseded by [ADR-012](./012-unified-table-architecture.md).
> The core concept of treating indexes as specialty tables remains, but the implementation has been
> unified into a single trait hierarchy with capability-based specialization. See ADR-012 for the
> complete unified architecture.

## Context

Nanostore needs to support secondary indexes for efficient query execution. The question is: should the transaction layer automatically maintain indexes when table data changes, or should index maintenance be explicit?

Two approaches were considered:

1. **Automatic Index Maintenance**: Transaction layer automatically updates all indexes when table data changes
   - Pros: Convenient for users, ensures consistency
   - Cons: Tight coupling, inflexible, complex transaction layer, limits index types

2. **Explicit Index Maintenance**: Indexes are treated as specialty tables, API consumers maintain them
   - Pros: Simple transaction layer, flexible strategies, supports any index type
   - Cons: More work for API consumers, potential for inconsistency if not careful

## Decision

We treat **indexes as specialty tables** at the transaction layer. The transaction layer provides explicit index operations (`index_get`, `index_put`, `index_delete`, `index_range_delete`) but does NOT automatically maintain indexes when table data changes.

Index maintenance is the responsibility of the API consumer (Database layer, query engine, etc.).

## Rationale

### 1. Separation of Concerns

The transaction layer should focus on ACID properties (atomicity, consistency, isolation, durability), not on understanding the relationship between tables and indexes. By treating indexes as specialty tables, the transaction layer remains simple and focused.

### 2. Flexibility in Index Maintenance Strategies

Different applications have different index maintenance requirements:

- **Synchronous**: Update indexes in the same transaction (strong consistency)
- **Deferred**: Update indexes after commit (eventual consistency, better write performance)
- **Asynchronous**: Update indexes in background (stale reads acceptable, best write performance)
- **Conditional**: Only update certain indexes based on query patterns

With explicit index operations, the Database layer can implement any of these strategies.

### 3. Support for Diverse Index Types

Nanostore supports many index types (B-Tree, LSM, Bloom filters, full-text, vector, geospatial, etc.). Each has different maintenance characteristics:

- Bloom filters are append-only
- Full-text indexes require tokenization
- Vector indexes may need periodic retraining
- Geospatial indexes have complex update logic

The transaction layer cannot reasonably understand all these index types. By making index operations explicit, we allow each index type to define its own maintenance logic at a higher layer.

### 4. Unified Object Model

Both tables and indexes use `ObjectId` at the transaction/storage layer. This provides:

- Uniform conflict detection
- Unified write-ahead logging
- Consistent lock management
- Simpler implementation

The distinction between tables and indexes is a logical concept at the Database layer, not a storage layer concern.

### 5. Explicit is Better Than Implicit

Making index maintenance explicit:

- Makes the cost visible (index updates appear in write set)
- Prevents hidden performance issues
- Allows fine-grained control over which indexes to update
- Makes it clear when indexes might be stale

## Implementation

### Transaction Layer API

```rust
pub trait TransactionOps {
    // Table operations
    fn get(&self, table: TableId, key: &[u8]) -> TransactionResult<Option<ValueBuf>>;
    fn put(&mut self, table: TableId, key: &[u8], value: &[u8]) -> TransactionResult<()>;
    fn delete(&mut self, table: TableId, key: &[u8]) -> TransactionResult<bool>;
    
    // Index operations (explicit, not automatic)
    fn index_get(&self, index: IndexId, key: &[u8]) -> TransactionResult<Option<ValueBuf>>;
    fn index_put(&mut self, index: IndexId, key: &[u8], value: &[u8]) -> TransactionResult<()>;
    fn index_delete(&mut self, index: IndexId, key: &[u8]) -> TransactionResult<bool>;
    
    // ... other methods
}
```

### Internal Representation

Both table and index operations use `ObjectId` internally:

```rust
pub struct Transaction {
    // Unified write set for both tables and indexes
    write_set: HashMap<(ObjectId, Vec<u8>), Option<Vec<u8>>>,
    // ... other fields
}
```

### Database Layer Responsibility

The Database layer implements index maintenance:

```rust
impl Database {
    pub fn put(&mut self, table: TableId, key: &[u8], value: &[u8]) -> Result<()> {
        let mut txn = self.begin_write()?;
        
        // Update table
        txn.put(table, key, value)?;
        
        // Update indexes (explicit)
        for index in self.get_indexes_for_table(table) {
            let index_key = index.extract_key(key, value)?;
            txn.index_put(index.id(), &index_key, key)?;
        }
        
        txn.commit()?;
        Ok(())
    }
}
```

## Consequences

### Positive

- **Simple transaction layer**: Focused on ACID, not index semantics
- **Flexible**: Supports any index maintenance strategy
- **Extensible**: New index types don't require transaction layer changes
- **Explicit costs**: Index updates visible in write set
- **Testable**: Can test table and index operations independently

### Negative

- **More work for Database layer**: Must implement index maintenance logic
- **Potential inconsistency**: If Database layer has bugs, indexes may be stale
- **Documentation burden**: Must clearly document that indexes aren't automatic

### Mitigation

1. Provide helper utilities in Database layer for common index maintenance patterns
2. Add comprehensive tests for index maintenance logic
3. Document index maintenance requirements clearly
4. Consider adding optional index consistency checks in debug builds

## Related Decisions

- [ADR 007: Unified Object ID](007-unified-object-id.md) - Provides the `ObjectId` abstraction used here
- [ADR 004: Multiple Storage Engines](004-multiple-storage-engines.md) - Index types are storage engines

## References

- Issue: Nanostore-j89 "Add index operations to Transaction layer"
- Implementation: `src/txn/transaction.rs`
- Tests: `tests/transaction_index_tests.rs`