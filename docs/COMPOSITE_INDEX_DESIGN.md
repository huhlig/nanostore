# Composite/Multi-Column Index Design

**Issue**: Nanostore-au0  
**Date**: 2026-05-12  
**Status**: Design Phase

## Executive Summary

This document outlines how to implement composite/multi-column indexes in Nanostore **without requiring schema support**. The key insight is that composite indexes can be implemented using **key encoding conventions** rather than schema metadata, leveraging the existing `DenseOrdered` trait and byte-oriented key-value storage.

## Problem Statement

We need to support indexes spanning multiple columns (e.g., `(tenant_id, user_id)`, `(last_name, first_name)`) for:
- Composite primary keys
- Multi-column sort optimization
- Partial index matching (WHERE tenant_id = ? AND user_id > ?)
- Covering indexes (index contains all query columns)
- Foreign key indexes

**Constraint**: No schema system exists or is planned. All key encoding must be self-describing or convention-based.

## Core Design Principle

**Composite indexes are just regular DenseOrdered indexes with specially encoded keys.**

The "composite" nature is entirely in the **key encoding**, not in the table type. This means:
- No new traits needed
- No schema metadata required
- Works with existing B-Tree and LSM implementations
- Database layer handles encoding/decoding

## Key Encoding Strategy

### 1. Tuple Encoding Format

Use a **self-describing tuple encoding** that preserves lexicographic ordering:

```
[field_count: u8][field_1_encoded][separator][field_2_encoded][separator]...[field_n_encoded]
```

**Field Encoding Rules:**
- **Integers**: Big-endian encoding (preserves order)
- **Strings**: UTF-8 bytes + length prefix or null terminator
- **Bytes**: Length prefix + raw bytes
- **Nulls**: Special marker byte (0x00 sorts first)
- **Descending**: Bitwise NOT of encoded value

**Separator**: Use `0xFF` as field separator (sorts after all valid UTF-8)

### 2. Example Encodings

**Composite key: (tenant_id: u32, user_id: u64)**
```
[0x02][BE(tenant_id)][0xFF][BE(user_id)]
```

**Composite key: (last_name: String, first_name: String)**
```
[0x02][len(last_name)][last_name_bytes][0xFF][len(first_name)][first_name_bytes]
```

**With NULL handling: (tenant_id: u32, email: Option<String>)**
```
// email is NULL
[0x02][BE(tenant_id)][0xFF][0x00]

// email is "alice@example.com"
[0x02][BE(tenant_id)][0xFF][0x01][len(email)][email_bytes]
```

**With descending order: (tenant_id ASC, created_at DESC)**
```
[0x02][BE(tenant_id)][0xFF][NOT(BE(created_at))]
```

### 3. Partial Matching Support

The encoding naturally supports partial matching:

```rust
// Full key: (tenant_id=42, user_id=1000)
let full_key = encode_composite(&[
    Field::U32(42),
    Field::U64(1000),
]);

// Prefix for tenant_id=42 (matches all user_ids)
let prefix = encode_composite(&[
    Field::U32(42),
]);

// Range scan: tenant_id=42 AND user_id >= 1000
let start = encode_composite(&[
    Field::U32(42),
    Field::U64(1000),
]);
let end = encode_composite(&[
    Field::U32(42),
    Field::U64(u64::MAX),
]);
```

## Implementation Architecture

### 1. No New Table Type Needed

Composite indexes use **existing table implementations**:
- `PagedBTree` for disk-backed indexes
- `MemoryBTree` for in-memory indexes
- `LsmTree` for write-heavy workloads

They implement `DenseOrdered` just like single-column indexes.

### 2. Key Encoding Module

Create a new module for composite key encoding:

```rust
// src/table/composite_key.rs

pub enum Field {
    Null,
    U32(u32),
    U64(u64),
    I32(i32),
    I64(i64),
    String(String),
    Bytes(Vec<u8>),
    // Add more types as needed
}

pub struct CompositeKeyEncoder {
    fields: Vec<Field>,
    descending: Vec<bool>,
}

impl CompositeKeyEncoder {
    pub fn new() -> Self { /* ... */ }
    
    pub fn add_field(&mut self, field: Field, descending: bool) { /* ... */ }
    
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.push(self.fields.len() as u8);
        
        for (i, field) in self.fields.iter().enumerate() {
            let encoded = self.encode_field(field);
            let final_bytes = if self.descending[i] {
                // Bitwise NOT for descending order
                encoded.iter().map(|b| !b).collect()
            } else {
                encoded
            };
            buf.extend_from_slice(&final_bytes);
            if i < self.fields.len() - 1 {
                buf.push(0xFF); // Separator
            }
        }
        
        buf
    }
    
    fn encode_field(&self, field: &Field) -> Vec<u8> {
        match field {
            Field::Null => vec![0x00],
            Field::U32(v) => {
                let mut buf = vec![0x01]; // Type marker
                buf.extend_from_slice(&v.to_be_bytes());
                buf
            }
            Field::U64(v) => {
                let mut buf = vec![0x02]; // Type marker
                buf.extend_from_slice(&v.to_be_bytes());
                buf
            }
            Field::String(s) => {
                let bytes = s.as_bytes();
                let mut buf = vec![0x03]; // Type marker
                buf.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
                buf.extend_from_slice(bytes);
                buf
            }
            // ... other types
        }
    }
    
    pub fn encode_prefix(&self, num_fields: usize) -> Vec<u8> {
        // Encode only first N fields for prefix matching
        let mut encoder = CompositeKeyEncoder::new();
        for i in 0..num_fields.min(self.fields.len()) {
            encoder.add_field(self.fields[i].clone(), self.descending[i]);
        }
        encoder.encode()
    }
}

pub struct CompositeKeyDecoder;

impl CompositeKeyDecoder {
    pub fn decode(bytes: &[u8]) -> Result<Vec<Field>, DecodeError> {
        // Parse field count
        let field_count = bytes[0] as usize;
        let mut fields = Vec::with_capacity(field_count);
        let mut pos = 1;
        
        for _ in 0..field_count {
            let (field, consumed) = Self::decode_field(&bytes[pos..])?;
            fields.push(field);
            pos += consumed;
            
            // Skip separator if not last field
            if pos < bytes.len() && bytes[pos] == 0xFF {
                pos += 1;
            }
        }
        
        Ok(fields)
    }
    
    fn decode_field(bytes: &[u8]) -> Result<(Field, usize), DecodeError> {
        match bytes[0] {
            0x00 => Ok((Field::Null, 1)),
            0x01 => {
                let value = u32::from_be_bytes(bytes[1..5].try_into()?);
                Ok((Field::U32(value), 5))
            }
            0x02 => {
                let value = u64::from_be_bytes(bytes[1..9].try_into()?);
                Ok((Field::U64(value), 9))
            }
            0x03 => {
                let len = u32::from_be_bytes(bytes[1..5].try_into()?) as usize;
                let s = String::from_utf8(bytes[5..5+len].to_vec())?;
                Ok((Field::String(s), 5 + len))
            }
            _ => Err(DecodeError::UnknownTypeMarker(bytes[0])),
        }
    }
}
```

### 3. Database Layer Integration

The Database layer uses composite key encoding when maintaining indexes:

```rust
impl Database {
    pub fn create_composite_index(
        &mut self,
        name: &str,
        parent_table: ObjectId,
        fields: Vec<IndexFieldSpec>,
    ) -> Result<ObjectId> {
        // Create a regular DenseOrdered table
        let options = TableOptions {
            engine: TableEngineKind::BTree,
            key_encoding: KeyEncoding::Custom(COMPOSITE_KEY_ENCODING_ID),
            // ... other options
        };
        
        let index_id = self.create_table(name, options)?;
        
        // Store field specifications in catalog metadata
        self.catalog.set_index_fields(index_id, fields)?;
        
        Ok(index_id)
    }
    
    pub fn insert_with_indexes(
        &mut self,
        table: ObjectId,
        key: &[u8],
        value: &[u8],
    ) -> Result<()> {
        let mut txn = self.begin_write()?;
        
        // Insert into table
        txn.put(table, key, value)?;
        
        // Update all indexes
        for index_id in self.indexes_for_table(table) {
            let field_specs = self.catalog.get_index_fields(index_id)?;
            
            // Extract field values from the record
            let fields = self.extract_fields(value, &field_specs)?;
            
            // Encode composite key
            let mut encoder = CompositeKeyEncoder::new();
            for (field, spec) in fields.iter().zip(field_specs.iter()) {
                encoder.add_field(field.clone(), spec.descending);
            }
            let index_key = encoder.encode();
            
            // Insert into index (index_key -> primary_key)
            txn.put(index_id, &index_key, key)?;
        }
        
        txn.commit()?;
        Ok(())
    }
    
    fn extract_fields(
        &self,
        value: &[u8],
        field_specs: &[IndexFieldSpec],
    ) -> Result<Vec<Field>> {
        // This is where you'd parse the value bytes to extract fields
        // The format depends on how you store records
        // Options:
        // 1. Fixed-offset encoding (requires knowing field sizes)
        // 2. Length-prefixed encoding (self-describing)
        // 3. MessagePack/CBOR/etc. (structured format)
        // 4. Application-provided extraction function
        
        // For now, assume application provides extraction logic
        todo!("Application-specific field extraction")
    }
}

pub struct IndexFieldSpec {
    pub field_index: usize,  // Which field in the record
    pub field_type: FieldType,
    pub descending: bool,
}

pub enum FieldType {
    U32,
    U64,
    I32,
    I64,
    String,
    Bytes,
}
```

### 4. Query Usage

```rust
// Create composite index on (tenant_id, user_id)
let index_id = db.create_composite_index(
    "users_tenant_user_idx",
    users_table_id,
    vec![
        IndexFieldSpec {
            field_index: 0,  // tenant_id
            field_type: FieldType::U32,
            descending: false,
        },
        IndexFieldSpec {
            field_index: 1,  // user_id
            field_type: FieldType::U64,
            descending: false,
        },
    ],
)?;

// Query: WHERE tenant_id = 42 AND user_id >= 1000
let mut encoder = CompositeKeyEncoder::new();
encoder.add_field(Field::U32(42), false);
encoder.add_field(Field::U64(1000), false);
let start_key = encoder.encode();

encoder = CompositeKeyEncoder::new();
encoder.add_field(Field::U32(42), false);
encoder.add_field(Field::U64(u64::MAX), false);
let end_key = encoder.encode();

let bounds = ScanBounds::Range {
    start: Bound::Included(KeyBuf(start_key)),
    end: Bound::Included(KeyBuf(end_key)),
};

let cursor = index.scan(bounds)?;
while cursor.valid() {
    let primary_key = cursor.primary_key();
    let record = table.get(primary_key)?;
    // Process record
    cursor.next()?;
}

// Query: WHERE tenant_id = 42 (prefix match)
let mut encoder = CompositeKeyEncoder::new();
encoder.add_field(Field::U32(42), false);
let prefix = encoder.encode();

let bounds = ScanBounds::Prefix(KeyBuf(prefix));
let cursor = index.scan(bounds)?;
```

## Advantages of This Approach

### 1. No Schema Required
- Key encoding is self-describing with type markers
- Field count stored in the key itself
- No separate schema metadata needed
- Works with any value format (JSON, MessagePack, raw bytes, etc.)

### 2. Leverages Existing Infrastructure
- Uses existing `DenseOrdered` trait
- Works with `PagedBTree`, `MemoryBTree`, `LsmTree`
- No changes to transaction layer
- No changes to storage layer

### 3. Flexible and Extensible
- Easy to add new field types
- Supports mixed ascending/descending orders
- Handles NULL values naturally
- Partial matching works automatically

### 4. Performance
- Lexicographic ordering preserved
- Efficient prefix scans
- No decoding needed for comparisons
- Compact encoding

### 5. Testable
- Encoding/decoding can be unit tested independently
- Index operations use standard `DenseOrdered` interface
- No complex integration points

## Implementation Plan

### Phase 1: Key Encoding Module
1. Create `src/table/composite_key.rs`
2. Implement `Field` enum with common types
3. Implement `CompositeKeyEncoder`
4. Implement `CompositeKeyDecoder`
5. Add comprehensive unit tests

### Phase 2: Catalog Integration
1. Add `index_fields` to `TableInfo` metadata
2. Implement field spec storage/retrieval
3. Add helper methods for composite index creation

### Phase 3: Database Layer Integration
1. Implement `create_composite_index()`
2. Implement field extraction (application-provided)
3. Update index maintenance logic
4. Add query helper methods

### Phase 4: Testing
1. Unit tests for encoding/decoding
2. Integration tests for index creation
3. Query tests for partial matching
4. Performance benchmarks

### Phase 5: Documentation
1. User guide for composite indexes
2. API documentation
3. Examples and best practices

## Alternative Approaches Considered

### Alternative 1: Schema-Based Approach
**Rejected**: Requires full schema system, which is out of scope.

### Alternative 2: Application-Level Encoding
**Rejected**: Pushes complexity to users, no standard format.

### Alternative 3: Fixed-Width Encoding
**Rejected**: Doesn't support variable-length fields (strings, bytes).

### Alternative 4: Separate Composite Index Type
**Rejected**: Unnecessary complexity, duplicates existing functionality.

## Open Questions

### 1. Field Extraction from Values
**Question**: How does the database extract field values from records?

**Options**:
- **A. Application-provided extraction function**: User supplies a closure
- **B. Structured format (MessagePack/CBOR)**: Parse structured data
- **C. Fixed-offset encoding**: Requires knowing field sizes upfront
- **D. Separate field storage**: Store fields separately from values

**Recommendation**: Start with **Option A** (application-provided), add **Option B** later.

### 2. Type Safety
**Question**: How do we ensure type consistency across index operations?

**Answer**: Store `FieldType` in catalog metadata, validate at runtime during encoding.

### 3. Null Handling
**Question**: Should NULLs sort first or last?

**Answer**: SQL standard is NULLs sort first. Use `0x00` marker.

### 4. Descending Order
**Question**: How to handle descending fields efficiently?

**Answer**: Bitwise NOT of encoded bytes. Simple and preserves ordering.

## Conclusion

Composite indexes can be implemented in Nanostore **without schema support** by using:
1. **Self-describing key encoding** with type markers
2. **Existing `DenseOrdered` trait** and table implementations
3. **Catalog metadata** for field specifications
4. **Database layer** for encoding/decoding logic

This approach is:
- ✅ Schema-free
- ✅ Flexible and extensible
- ✅ Leverages existing infrastructure
- ✅ Performant
- ✅ Testable

The key insight is that **composite indexes are just regular indexes with specially encoded keys**. The "composite" nature is entirely in the encoding, not in the table type.

## Next Steps

1. Review this design with the team
2. Get feedback on field extraction approach
3. Implement Phase 1 (key encoding module)
4. Create prototype with simple example
5. Iterate based on feedback

---

**Author**: Bob (AI Assistant)  
**Reviewers**: Hans W. Uhlig  
**Last Updated**: 2026-05-12