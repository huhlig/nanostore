# ADR-001: Single-File Database Design

**Status**: Accepted  
**Date**: 2026-05-10  
**Deciders**: Hans W. Uhlig, Development Team  
**Technical Story**: Core architecture decision

## Context

Nanostore needs a storage architecture that balances simplicity, portability, and performance. Traditional databases often use multiple files (data files, index files, log files, etc.), which can complicate deployment, backup, and management.

Key requirements:
- Easy deployment and distribution
- Simple backup and restore
- Atomic file operations
- Cross-platform compatibility
- Embeddable design

## Decision

We will use a **single-file database design** where all data, indexes, and metadata are stored in one file.

**File Structure**:
- One `.db` file for all persistent data
- One `.wal` file for write-ahead log (temporary, can be deleted after checkpoint)
- All pages, indexes, and metadata in the `.db` file

**Benefits**:
1. **Simplicity**: One file to manage, backup, and restore
2. **Portability**: Easy to copy, move, or distribute
3. **Atomic operations**: File-level operations are atomic
4. **Embeddability**: No complex file management
5. **SQLite compatibility**: Familiar model for users

## Consequences

### Positive

- **Easy deployment**: Just copy one file
- **Simple backup**: Copy the file (with proper locking)
- **Easy testing**: Create/delete test databases easily
- **Portable**: Works across platforms without changes
- **Familiar**: SQLite-like model is well understood

### Negative

- **File size limits**: Single file can grow large
- **Concurrent access**: File-level locking may limit concurrency
- **Fragmentation**: File fragmentation can impact performance
- **Backup complexity**: Must ensure consistent state during backup

### Mitigations

1. **File size**: Support up to 1TB+ files (sufficient for most use cases)
2. **Concurrency**: Use page-level locking and MVCC for parallelism
3. **Fragmentation**: Use free list and page reuse to minimize fragmentation
4. **Backup**: Provide checkpoint mechanism for consistent backups

## Alternatives Considered

### Alternative 1: Multi-File Design

**Structure**: Separate files for data, indexes, logs, etc.

**Pros**:
- Better organization
- Easier to manage individual components
- Can optimize per-file settings

**Cons**:
- More complex deployment
- Harder to backup atomically
- More file handles needed
- Coordination between files required

**Rejected because**: Complexity outweighs benefits for an embedded database.

### Alternative 2: Directory-Based Design

**Structure**: One directory per database with multiple files inside.

**Pros**:
- Organized structure
- Easy to add new file types
- Can use file system features

**Cons**:
- Directory management complexity
- Harder to distribute
- Platform-specific directory handling
- Not truly "single file"

**Rejected because**: Defeats the purpose of simplicity.

### Alternative 3: Append-Only Log

**Structure**: Single append-only log file with compaction.

**Pros**:
- Simple write path
- Good for write-heavy workloads
- Easy to implement

**Cons**:
- Poor read performance
- Requires frequent compaction
- No random access
- Not suitable for general-purpose database

**Rejected because**: Read performance is critical for most workloads.

## Implementation Details

### File Layout

```
Database File (.db):
┌─────────────────────────────────────────┐
│ Page 0: File Header                     │
│  - Magic number, version, config        │
├─────────────────────────────────────────┤
│ Page 1: Superblock                      │
│  - Metadata, free list, root pointers   │
├─────────────────────────────────────────┤
│ Page 2+: Data Pages                     │
│  - B-Tree nodes, LSM SSTables, etc.     │
└─────────────────────────────────────────┘

WAL File (.wal):
┌─────────────────────────────────────────┐
│ WAL Header                              │
├─────────────────────────────────────────┤
│ Transaction Records                     │
│  - BEGIN, WRITE, COMMIT, etc.           │
└─────────────────────────────────────────┘
```

### File Operations

**Create**:
```rust
Pager::create(fs, "database.db", config)
  → Create file
  → Write header (page 0)
  → Write superblock (page 1)
  → Sync to disk
```

**Open**:
```rust
Pager::open(fs, "database.db")
  → Open file
  → Read header (page 0)
  → Validate magic number and version
  → Read superblock (page 1)
  → Check for WAL and recover if needed
```

**Backup**:
```rust
Database::checkpoint()
  → Flush all dirty pages
  → Sync to disk
  → Copy .db file (now consistent)
```

## Performance Considerations

### File Size Growth

- **Initial size**: 2 pages (8KB with 4KB pages)
- **Growth**: Allocate pages as needed
- **Maximum**: Limited by file system (typically 16TB+)
- **Typical**: 1-100 GB for most applications

### I/O Patterns

- **Sequential writes**: WAL provides sequential write performance
- **Random reads**: Page cache minimizes disk I/O
- **Random writes**: Batched via WAL, then applied to pages

### Concurrency

- **File lock**: Exclusive for writes, shared for reads (VFS level)
- **Page locks**: Fine-grained locking within file (64 shards)
- **MVCC**: Non-blocking reads via version chains

## Monitoring and Metrics

Track these metrics:
- File size growth rate
- Page allocation rate
- Free list size
- Fragmentation ratio
- I/O operations per second

## References

- [SQLite File Format](https://www.sqlite.org/fileformat.html)
- [LMDB Architecture](http://www.lmdb.tech/doc/)
- [File Format Specification](../FILE_FORMAT.md)
- [Architecture Overview](../ARCHITECTURE.md)

## Related ADRs

- [ADR-002: Page-Based Storage](./002-page-based-storage.md)
- [ADR-005: Write-Ahead Logging](./005-write-ahead-logging.md)
- [ADR-009: VFS Abstraction](./009-vfs-abstraction.md)

---

**Last Updated**: 2026-05-10