# Architecture Decision Records (ADRs)

This directory contains Architecture Decision Records (ADRs) documenting key design decisions made during the development of Nanostore.

## ADR Index

| ADR | Title | Status | Date |
|-----|-------|--------|------|
| [001](./001-single-file-database.md) | Single-File Database Design | Accepted | 2026-05-10 |
| [002](./002-page-based-storage.md) | Page-Based Storage Architecture | Proposed | 2026-05-10 |
| [003](./003-mvcc-concurrency.md) | MVCC Concurrency Control | Accepted | 2026-05-10 |
| [004](./004-multiple-storage-engines.md) | Multiple Storage Engines | Accepted | 2026-05-10 |
| [005](./005-write-ahead-logging.md) | Write-Ahead Logging for Durability | Proposed | 2026-05-10 |
| [006](./006-sharded-concurrency.md) | Sharded Concurrency Model | Accepted | 2026-05-10 |
| [007](./007-unified-object-id.md) | Unified ObjectId System | Superseded | 2026-05-10 |
| [008](./008-optional-compression-encryption.md) | Optional Compression and Encryption | Proposed | 2026-05-10 |
| [009](./009-vfs-abstraction.md) | Virtual File System Abstraction | Proposed | 2026-05-10 |
| [010](./010-lsm-bloom-filters.md) | Bloom Filters for LSM Trees | Proposed | 2026-05-10 |
| [011](./011-indexes-as-specialty-tables.md) | Indexes as Specialty Tables | Superseded | 2026-05-10 |
| [012](./012-unified-table-architecture.md) | Unified Table Architecture | Accepted | 2026-05-11 |

## ADR Format

Each ADR follows this structure:

```markdown
# ADR-XXX: Title

**Status**: Proposed | Accepted | Deprecated | Superseded  
**Date**: YYYY-MM-DD  
**Deciders**: Names  
**Technical Story**: Issue/ticket reference

## Context

What is the issue we're facing?

## Decision

What decision did we make?

## Consequences

What are the positive and negative consequences?

## Alternatives Considered

What other options did we consider?

## References

Links to related documents, discussions, or code.
```

## Status Definitions

- **Proposed**: Under discussion
- **Accepted**: Decision made and implemented
- **Deprecated**: No longer relevant
- **Superseded**: Replaced by another ADR

## Related Documents

- [Architecture Overview](../ARCHITECTURE.md)
- [File Format Specification](../FILE_FORMAT.md)
- [Design Decisions Archive](../archive/DESIGN_DECISIONS.md)

---

**Last Updated**: 2026-05-10