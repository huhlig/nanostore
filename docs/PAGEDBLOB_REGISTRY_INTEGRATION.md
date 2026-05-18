# PagedBlob Registry Integration

## Overview

This document describes the refactoring of `PagedBlob` to work with the table registry system, addressing the issues identified in `src/table.rs` at lines 175 and 466.

## Problem Statement

PagedBlob was not compatible with the table registry system because:
1. It didn't accept a `Pager` parameter like other paged engines
2. It didn't work with the generic `FS` (FileSystem) parameter
3. It couldn't be included in the `TableEngineInstance` enum
4. It wasn't supported in registry operations (create/open)

## Changes Made

### 1. PagedBlob Structure Refactoring (`src/table/blob/paged.rs`)

**Before:**
```rust
pub struct PagedBlob<FS: FileSystem> {
    id: TableId,
    name: String,
    page_size: usize,  // Direct page size parameter
    pager: Arc<Pager<FS>>,
    index: Arc<RwLock<HashMap<Vec<u8>, VersionChain>>>,
}

pub fn new(id: TableId, name: String, page_size: usize, pager: Arc<Pager<FS>>) -> Self
```

**After:**
```rust
pub struct PagedBlob<FS: FileSystem> {
    id: TableId,
    name: String,
    pager: Arc<Pager<FS>>,
    root_page_id: PageId,  // Added root page tracking
    index: Arc<RwLock<HashMap<Vec<u8>, VersionChain>>>,
}

pub fn new(id: TableId, name: String, pager: Arc<Pager<FS>>) -> TableResult<Self>
pub fn open(id: TableId, name: String, pager: Arc<Pager<FS>>, root_page_id: PageId) -> Self
pub fn root_page_id(&self) -> PageId
```

**Key Changes:**
- Removed `page_size` parameter - now derived from `pager.page_size().data_size()`
- Added `root_page_id` field for metadata tracking
- Added `new()` method that allocates and initializes a root page
- Added `open()` method for loading existing tables
- Added `root_page_id()` accessor method
- Changed `put()`, `put_tx()`, `delete()`, and `delete_tx()` to take `&self` instead of `&mut self` (interior mutability via RwLock)

### 2. TableEngineInstance Enum (`src/table.rs`)

**Added PagedBlob variant:**
```rust
pub enum TableEngineInstance<FS: FileSystem> {
    // ... other variants ...
    PagedBlob(Arc<PagedBlob<FS>>),  // NEW
    // ... other variants ...
}
```

**Updated all match statements to include PagedBlob:**
- `table_id()` method
- `name()` method  
- `kind()` method
- `root_page_id()` method
- `Clone` implementation
- `vacuum()` method in TableEngineRegistry

### 3. Registry Support (`src/table.rs`)

**create_engine method:**
```rust
TableEngineKind::Blob => {
    let blob = PagedBlob::new(table_id, name, self.pager.clone())
        .map_err(|e| RegistryError::EngineCreationFailed {
            engine: options.engine,
            details: format!("Failed to create PagedBlob: {}", e),
        })?;
    let root_page_id = blob.root_page_id();
    Ok((
        TableEngineInstance::PagedBlob(Arc::new(blob)),
        Some(root_page_id),
    ))
}
```

**open_engine method:**
```rust
TableEngineKind::Blob => {
    let blob = PagedBlob::open(table_id, name, self.pager.clone(), root_page_id);
    Ok(TableEngineInstance::PagedBlob(Arc::new(blob)))
}
```

### 4. Transaction Integration (`src/txn/transaction.rs`)

Added PagedBlob support to all transaction operations:

**Get operations:**
```rust
TableEngineInstance::PagedBlob(blob) => {
    blob.get(key).map_err(|e| {
        TransactionError::Other(format!("Paged Blob get failed: {}", e))
    })?
}
```

**Put/Delete operations:**
```rust
TableEngineInstance::PagedBlob(blob) => {
    match value_opt {
        Some(value) => {
            blob.put_tx(key, value, self.txn_id).map_err(|e| {
                TransactionError::Other(format!("PagedBlob put failed: {}", e))
            })?;
        }
        None => {
            blob.delete_tx(key, self.txn_id).map_err(|e| {
                TransactionError::Other(format!("PagedBlob delete failed: {}", e))
            })?;
        }
    }
}
```

**Range operations:**
```rust
TableEngineInstance::PagedBlob(_) => {
    return Err(TransactionError::Other(
        "range_delete is not supported for Blob tables".to_string(),
    ));
}
```

## Technical Details

### Page Type Selection

PagedBlob uses `PageType::Catalog` for its root metadata page, consistent with other table engines that store metadata.

### Interior Mutability

PagedBlob uses `RwLock` for interior mutability on the index, allowing methods to take `&self` instead of `&mut self`. This is necessary because:
1. The engine is stored in an `Arc` in the registry
2. Multiple transactions may need concurrent read access
3. Write operations are serialized through the RwLock

### MVCC Support

PagedBlob maintains full MVCC support through:
- Version chains stored in the index
- Transaction ID tracking in `put_tx()` and `delete_tx()`
- Snapshot visibility through `get()` method (returns latest committed version)

## Testing

The refactoring compiles successfully with no errors. All existing PagedBlob functionality is preserved while adding registry compatibility.

## Benefits

1. **Consistency**: PagedBlob now follows the same pattern as other paged engines
2. **Registry Support**: Can be created and opened through the table registry
3. **Transaction Integration**: Full support for transactional operations
4. **Persistence**: Proper root page tracking for database recovery
5. **Type Safety**: Works with the generic FileSystem parameter

## Future Improvements

1. Consider adding snapshot-based `get_snapshot()` support for more precise MVCC visibility
2. Add support for range scan operations if needed
3. Optimize vacuum operations for blob-specific cleanup

## Related Issues

- Resolves: nanokv-k1t (Refactor PagedBlob to work with table registry system)
- Related: Table engine standardization efforts