# Phase 4: Error Handling & Recovery - Improvements

## Overview

This document describes the error handling improvements made during Phase 4 of the Nanostore project. The focus was on replacing generic error variants with structured, context-rich error types that provide better debuggability and operational visibility.

## Changes Made

### 1. New TableError Variants

Added six new structured error variants to `TableError` enum:

```rust
/// Invalid level in LSM tree
InvalidLevel {
    level: u32,
    max_level: u32,
}

/// SSTable ID already exists
SStableIdExists {
    id: String,
}

/// SSTable ID not found
SStableIdNotFound {
    id: String,
}

/// Manifest operation error
ManifestError {
    operation: String,
    details: String,
}

/// Invalid operation state
InvalidOperationState {
    operation: String,
    reason: String,
}

/// Serialization error
SerializationError {
    data_type: String,
    details: String,
}
```

### 2. Helper Methods

Added convenience methods for creating these errors:

- `TableError::invalid_level(level, max_level)`
- `TableError::sstable_id_exists(id)`
- `TableError::sstable_id_not_found(id)`
- `TableError::manifest_error(operation, details)`
- `TableError::invalid_operation_state(operation, reason)`
- `TableError::serialization_error(data_type, details)`

### 3. Error Observability Integration

Updated `error_observability.rs` to classify the new error variants:

- `InvalidLevel` → validation/invalid_level (Error)
- `SStableIdExists` → consistency/sstable_id_exists (Error)
- `SStableIdNotFound` → not_found/sstable_id_not_found (Warning)
- `ManifestError` → manifest/manifest_error (Error)
- `InvalidOperationState` → state/invalid_operation_state (Error)
- `SerializationError` → encoding/serialization_error (Error)

### 4. Code Updates

#### LSM Manifest (`src/table/lsm/manifest.rs`)

Replaced 7 instances of `TableError::Other` with structured variants:

1. **Invalid level check**: Now uses `TableError::invalid_level(level, max_level)`
2. **Duplicate SSTable ID**: Now uses `TableError::sstable_id_exists(id)`
3. **SSTable ID not found**: Now uses `TableError::sstable_id_not_found(id)`
4. **Serialization size limits**: Now uses `TableError::manifest_error(operation, details)`
5. **Manifest snapshot serialization**: Now uses `TableError::serialization_error(data_type, details)`
6. **Page size validation**: Now uses `TableError::manifest_error(operation, details)`
7. **Header size validation**: Now uses `TableError::manifest_error(operation, details)`
8. **Page count validation**: Now uses `TableError::manifest_error(operation, details)`

#### SSTable Builder (`src/table/lsm/sstable.rs`)

Replaced 5 instances of `TableError::Other` with structured variants:

1. **DataBlock unsorted keys**: Now uses `TableError::invalid_operation_state(operation, reason)`
2. **Version chain serialization**: Now uses `TableError::serialization_error(data_type, details)`
3. **IndexBlock unsorted entries**: Now uses `TableError::invalid_operation_state(operation, reason)`
4. **SStableBuilder unsorted keys**: Now uses `TableError::invalid_operation_state(operation, reason)`
5. **Empty SSTable creation**: Now uses `TableError::invalid_operation_state(operation, reason)`

## Benefits

### 1. Better Debuggability

Errors now include specific context:
- **Before**: `TableError::Other("Invalid level 5, max is 4")`
- **After**: `TableError::InvalidLevel { level: 5, max_level: 4 }`

### 2. Structured Logging

Errors can be logged with structured fields for better filtering and aggregation:

```rust
let err = TableError::invalid_level(5, 4);
let observation = err.record();
// Emits metrics with labels: subsystem=table, category=validation, variant=invalid_level
```

### 3. Type-Safe Error Handling

Code can pattern match on specific error types:

```rust
match result {
    Err(TableError::SStableIdNotFound { id }) => {
        // Handle missing SSTable specifically
    }
    Err(TableError::InvalidLevel { level, max_level }) => {
        // Handle invalid level specifically
    }
    _ => {}
}
```

### 4. Operational Visibility

Metrics are emitted with stable labels for monitoring:
- `Nanostore.error.total{subsystem="table", category="validation", variant="invalid_level"}`
- `Nanostore.error.total{subsystem="table", category="consistency", variant="sstable_id_exists"}`

## Testing

All 238 unit tests pass with the new error handling:
```
test result: ok. 238 passed; 0 failed; 0 ignored; 0 measured
```

Specific error handling tests validated:
- Index block corruption detection (checksum mismatch, truncated data, unsorted entries)
- Data block corruption detection
- Manifest validation errors

## Remaining Work

The following areas still use generic error variants and could benefit from similar improvements:

1. **Pager module**: `PagerError::InternalError` used in page deserialization (6 instances)
2. **WAL module**: `WalError::InternalError` used in commit notifications (2 instances)

These are lower priority as they occur in less frequently executed code paths.

## Related Documentation

- [Error Context Improvements](ERROR_CONTEXT_IMPROVEMENTS.md) - Original analysis and plan
- [Error Observability](../src/error_observability.rs) - Centralized error classification
- [Table Errors](../src/table/error.rs) - Complete error type definitions

## Conclusion

Phase 4 error handling improvements provide:
- ✅ Structured error variants with rich context
- ✅ Helper methods for ergonomic error creation
- ✅ Integrated observability with metrics and logging
- ✅ All tests passing
- ✅ Better debuggability for production issues

The improvements maintain backward compatibility while significantly enhancing error reporting quality.