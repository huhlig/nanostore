# Nanostore File Format Specification

**Version**: 1.0  
**Date**: 2026-05-10  
**Status**: Current

---

## Table of Contents

1. [Overview](#overview)
2. [Database File Format](#database-file-format)
3. [WAL File Format](#wal-file-format)
4. [Page Format](#page-format)
5. [Data Structures](#data-structures)
6. [Encoding Schemes](#encoding-schemes)
7. [Checksums and Integrity](#checksums-and-integrity)
8. [Version Compatibility](#version-compatibility)

---

## Overview

Nanostore uses two primary file types:
1. **Database file** (`.db`): Main data storage with page-based structure
2. **WAL file** (`.wal`): Write-ahead log for durability and recovery

Both files use little-endian byte order for all multi-byte integers.

---

## Database File Format

### File Structure

```
Offset    Size      Description
------    ----      -----------
0         PageSize  Page 0: File Header
PageSize  PageSize  Page 1: Superblock
2*PS      PageSize  Page 2: First data page
...       ...       ...
N*PS      PageSize  Page N: Last page
```

Where `PS` = Page Size (4096, 8192, 16384, 32768, or 65536 bytes)

### Page 0: File Header

**Size**: Exactly one page (page size bytes)

```
Offset  Size  Type    Description
------  ----  ----    -----------
0       8     u8[8]   Magic number: "Nanostore\0\0" (0x4E414E4F4B560000)
8       8     u64     Format version (currently 1)
16      4     u32     Page size (4096/8192/16384/32768/65536)
20      1     u8      Compression type (0=None, 1=LZ4, 2=Zstd)
21      1     u8      Encryption type (0=None, 1=AES-256-GCM)
22      2     u16     Reserved (must be 0)
24      32    u8[32]  Encryption salt (if encryption enabled)
56      8     u64     Creation timestamp (Unix epoch)
64      8     u64     Last modified timestamp
72      184   u8[184] Reserved (must be 0)
256     PS-256 u8[]   Padding (zeros)
```

**Notes**:
- Magic number identifies file as Nanostore database
- Version allows future format changes
- Encryption salt used for key derivation
- Timestamps in seconds since Unix epoch

### Page 1: Superblock

**Size**: Exactly one page (page size bytes)

The superblock is stored as page data with standard page header and checksum.

**Superblock Data Layout**:

```
Offset  Size  Type    Description
------  ----  ----    -----------
0       8     u64     Magic number: 0x4E4B5355504552 ("NKSUPER")
8       8     u64     Superblock version (currently 1)
16      8     u64     Total allocated pages
24      8     u64     Total free pages
32      8     u64     First free list page ID (0 if none)
40      8     u64     Last free list page ID (0 if none)
48      8     u64     Next page ID to allocate
56      8     u64     Transaction counter
64      8     u64     Last checkpoint LSN
72      8     u64     Root B-Tree page ID (0 if empty)
80      8     u64     Reserved
88      8     u64     Reserved
96      32    u8[32]  Reserved (must be 0)
128     ...   ...     Padding to fill page data section
```

**Notes**:
- Superblock updated on every transaction commit
- Next page ID uses atomic operations for thread safety
- Free list pages form a linked list
- Root B-Tree page is entry point for table data

---

## WAL File Format

### File Structure

```
Offset    Size      Description
------    ----      -----------
0         256       WAL Header
256       Variable  Record 1
...       Variable  Record 2
...       Variable  Record N
```

### WAL Header

```
Offset  Size  Type    Description
------  ----  ----    -----------
0       8     u8[8]   Magic number: "NANOKWAL" (0x4E414E4F4B57414C)
8       8     u64     Format version (currently 1)
16      8     u64     Start LSN (first record LSN)
24      8     u64     Creation timestamp
32      8     u64     Last write timestamp
40      216   u8[216] Reserved (must be 0)
```

### WAL Record Format

Each record has the following structure:

```
Offset  Size      Type    Description
------  ----      ----    -----------
0       8         u64     LSN (Log Sequence Number)
8       8         u64     Transaction ID
16      1         u8      Record type (see below)
17      3         u8[3]   Reserved (must be 0)
20      4         u32     Data length (N)
24      N         u8[N]   Record data (type-specific)
24+N    32        u8[32]  SHA-256 checksum
```

**Record Types**:

```
Value  Name       Description
-----  ----       -----------
0      BEGIN      Start transaction
1      WRITE      Write operation (put/delete)
2      COMMIT     Commit transaction
3      ROLLBACK   Rollback transaction
4      CHECKPOINT Checkpoint marker
```

### Record Data Formats

#### BEGIN Record

```
Offset  Size  Type    Description
------  ----  ----    -----------
0       8     u64     Transaction ID (redundant, for verification)
8       8     u64     Timestamp
```

#### WRITE Record

```
Offset  Size  Type    Description
------  ----  ----    -----------
0       8     u64     Table ID
8       1     u8      Operation type (0=Put, 1=Delete)
9       3     u8[3]   Reserved
12      4     u32     Key length (K)
16      K     u8[K]   Key data
16+K    4     u32     Value length (V, 0 for delete)
20+K    V     u8[V]   Value data (empty for delete)
```

#### COMMIT Record

```
Offset  Size  Type    Description
------  ----  ----    -----------
0       8     u64     Transaction ID
8       8     u64     Commit timestamp
```

#### ROLLBACK Record

```
Offset  Size  Type    Description
------  ----  ----    -----------
0       8     u64     Transaction ID
8       8     u64     Rollback timestamp
```

#### CHECKPOINT Record

```
Offset  Size  Type    Description
------  ----  ----    -----------
0       8     u64     Checkpoint LSN
8       4     u32     Active transaction count (N)
12      N*8   u64[N]  Active transaction IDs
```

---

## Page Format

### Standard Page Structure

Every page (except page 0) follows this structure:

```
┌─────────────────────────────────────────┐
│ Page Header (32 bytes)                  │
├─────────────────────────────────────────┤
│ Page Data (PageSize - 64 bytes)        │
│  - Compressed if compression enabled    │
│  - Encrypted if encryption enabled      │
├─────────────────────────────────────────┤
│ Checksum (32 bytes)                     │
│  - SHA-256 of header + data             │
└─────────────────────────────────────────┘
```

### Page Header

```
Offset  Size  Type    Description
------  ----  ----    -----------
0       8     u64     Page ID
8       1     u8      Page type (see below)
9       1     u8      Flags (see below)
10      2     u16     Reserved
12      4     u32     Data length (uncompressed)
16      4     u32     Compressed length (0 if not compressed)
20      4     u32     CRC32 of header (for quick validation)
24      8     u64     Reserved
```

**Page Types**:

```
Value  Name           Description
-----  ----           -----------
0      Header         File header (page 0 only)
1      Superblock     Database metadata (page 1 only)
2      FreeList       Free page list
3      BTreeInternal  B-Tree internal node
4      BTreeLeaf      B-Tree leaf node
5      LSMMemtable    LSM memtable data
6      LSMSSTable     LSM sorted string table
7      Data           Generic data page
```

**Flags** (bitfield):

```
Bit  Description
---  -----------
0    Compressed (1 if data is compressed)
1    Encrypted (1 if data is encrypted)
2    Dirty (1 if modified, not yet flushed)
3-7  Reserved (must be 0)
```

### Page Data Formats

#### Free List Page

```
Offset  Size  Type    Description
------  ----  ----    -----------
0       8     u64     Next free list page ID (0 if last)
8       4     u32     Entry count (N)
12      N*8   u64[N]  Free page IDs
```

#### B-Tree Internal Node

```
Offset  Size  Type    Description
------  ----  ----    -----------
0       1     u8      Node type (0=Internal)
1       1     u8      Reserved
2       2     u16     Entry count (N)
4       4     u32     Reserved
8       8     u64     Rightmost child page ID
16      ...   ...     Entries (see below)
```

**Internal Node Entry**:

```
Offset  Size  Type    Description
------  ----  ----    -----------
0       2     u16     Key length (K)
2       K     u8[K]   Key data
2+K     8     u64     Child page ID
```

#### B-Tree Leaf Node

```
Offset  Size  Type    Description
------  ----  ----    -----------
0       1     u8      Node type (1=Leaf)
1       1     u8      Reserved
2       2     u16     Entry count (N)
4       4     u32     Reserved
8       8     u64     Next leaf page ID (0 if last)
16      ...   ...     Entries (see below)
```

**Leaf Node Entry**:

```
Offset  Size  Type    Description
------  ----  ----    -----------
0       2     u16     Key length (K)
2       K     u8[K]   Key data
2+K     4     u32     Version chain length (V)
6+K     ...   ...     Version chain (V versions)
```

**Version Chain Entry**:

```
Offset  Size  Type    Description
------  ----  ----    -----------
0       8     u64     LSN
8       8     u64     Transaction ID
16      4     u32     Value length (L, 0 for delete)
20      L     u8[L]   Value data (empty for delete)
```

#### LSM SSTable Data Block

```
Offset  Size  Type    Description
------  ----  ----    -----------
0       4     u32     Entry count (N)
4       ...   ...     Entries (see below)
```

**SSTable Entry**:

```
Offset  Size  Type    Description
------  ----  ----    -----------
0       2     u16     Key length (K)
2       K     u8[K]   Key data
2+K     8     u64     LSN
10+K    8     u64     Transaction ID
18+K    4     u32     Value length (V, 0 for delete)
22+K    V     u8[V]   Value data (empty for delete)
```

#### LSM SSTable Index Block

```
Offset  Size  Type    Description
------  ----  ----    -----------
0       4     u32     Block count (N)
4       ...   ...     Block entries (see below)
```

**Index Block Entry**:

```
Offset  Size  Type    Description
------  ----  ----    -----------
0       8     u64     Block offset in file
8       4     u32     Block size (compressed)
12      4     u32     Block size (uncompressed)
16      2     u16     First key length (K)
18      K     u8[K]   First key in block
```

#### LSM SSTable Footer

```
Offset  Size  Type    Description
------  ----  ----    -----------
0       8     u64     Index block offset
8       4     u32     Index block size
12      8     u64     Bloom filter offset
20      4     u32     Bloom filter size
24      2     u16     Min key length (K1)
26      K1    u8[K1]  Min key
26+K1   2     u16     Max key length (K2)
28+K1   K2    u8[K2]  Max key
28+K1+K2 8    u64     Entry count
36+K1+K2 8    u64     Creation timestamp
44+K1+K2 32   u8[32]  Checksum (SHA-256)
```

---

## Data Structures

### Bloom Filter Format

```
Offset  Size  Type    Description
------  ----  ----    -----------
0       8     u64     Bit array size (N bits)
8       1     u8      Hash function count (K)
9       7     u8[7]   Reserved
16      N/8   u8[]    Bit array (N bits, packed)
```

**Hash Functions**: Uses K independent hash functions derived from SHA-256.

### Version Chain

Version chains are stored inline in B-Tree leaf nodes and LSM entries.

```
Version 1 (newest) → Version 2 → Version 3 (oldest)
```

Each version contains:
- LSN (when created)
- Transaction ID (who created it)
- Value (or tombstone for delete)

Garbage collection removes versions older than the oldest active snapshot.

---

## Encoding Schemes

### Integer Encoding

All integers use little-endian byte order:
- `u8`: 1 byte, unsigned
- `u16`: 2 bytes, unsigned, little-endian
- `u32`: 4 bytes, unsigned, little-endian
- `u64`: 8 bytes, unsigned, little-endian

### String Encoding

Strings are length-prefixed:
```
[2 bytes: length][N bytes: UTF-8 data]
```

### Key Encoding

Keys are stored as raw bytes with length prefix:
```
[2 bytes: length][K bytes: key data]
```

Maximum key length: 65535 bytes

### Value Encoding

Values are stored as raw bytes with length prefix:
```
[4 bytes: length][V bytes: value data]
```

Maximum value length: 4,294,967,295 bytes (4 GB)

---

## Checksums and Integrity

### SHA-256 Checksums

Used for:
- Page checksums (32 bytes at end of each page)
- WAL record checksums (32 bytes at end of each record)
- SSTable footer checksums

**Checksum Calculation**:
```
checksum = SHA-256(header || data)
```

### CRC32 Quick Validation

Page headers include CRC32 for quick validation:
```
crc32 = CRC32(page_header[0:20])
```

Allows fast detection of corruption without full SHA-256 verification.

### Verification Process

1. **Quick check**: Verify CRC32 of page header
2. **Full check**: Verify SHA-256 of entire page
3. **On mismatch**: Report corruption, attempt recovery

---

## Compression

### LZ4 Compression

- Fast compression/decompression
- Moderate compression ratio (~2-3x)
- Used for: Page data, SSTable blocks

**Format**:
```
[4 bytes: uncompressed size][N bytes: LZ4 compressed data]
```

### Zstd Compression

- Slower but better compression
- High compression ratio (~3-5x)
- Used for: Page data, SSTable blocks

**Format**:
```
[4 bytes: uncompressed size][N bytes: Zstd compressed data]
```

**Compression Level**: Default level 3 (balanced)

---

## Encryption

### AES-256-GCM

- Authenticated encryption
- 256-bit key
- 96-bit nonce (IV)
- 128-bit authentication tag

**Format**:
```
[12 bytes: nonce][N bytes: ciphertext][16 bytes: auth tag]
```

**Key Derivation**:
```
key = PBKDF2-HMAC-SHA256(password, salt, 100000 iterations)
```

Salt stored in file header (32 bytes).

**Nonce Generation**:
- Page encryption: `nonce = page_id || counter`
- WAL encryption: `nonce = lsn || counter`

---

## Version Compatibility

### Format Version 1

Current version. Features:
- Page-based storage
- MVCC with version chains
- Optional compression (LZ4, Zstd)
- Optional encryption (AES-256-GCM)
- SHA-256 checksums
- WAL for durability

### Future Versions

Version changes will be backward compatible when possible:
- New page types can be added
- New compression algorithms can be added
- New encryption algorithms can be added
- Format version in header allows detection

### Upgrade Path

When opening older format:
1. Detect version from file header
2. Read using old format
3. Optionally upgrade to new format
4. Update version in header

### Downgrade Protection

Newer formats cannot be opened by older code:
- Version check on open
- Error if version > supported version
- Prevents data corruption

---

## File Size Limits

### Theoretical Limits

- **Maximum pages**: 2^64 - 1 (18 quintillion)
- **Maximum file size**: PageSize × 2^64
  - 4KB pages: 73 exabytes
  - 64KB pages: 1.2 zettabytes
- **Maximum key size**: 65535 bytes (64 KB)
- **Maximum value size**: 4 GB
- **Maximum transaction ID**: 2^64 - 1

### Practical Limits

Recommended limits for production:
- **File size**: < 1 TB (for reasonable backup/recovery times)
- **Key size**: < 1 KB (for cache efficiency)
- **Value size**: < 1 MB (for memory efficiency)
- **Active transactions**: < 10,000 (for conflict detection)

---

## Error Detection and Recovery

### Corruption Detection

1. **CRC32 mismatch**: Page header corrupted
2. **SHA-256 mismatch**: Page data corrupted
3. **Magic number mismatch**: Wrong file type
4. **Version mismatch**: Incompatible format

### Recovery Strategies

1. **WAL replay**: Recover from last checkpoint
2. **Page reconstruction**: Rebuild from valid pages
3. **Backup restore**: Restore from backup
4. **Manual repair**: Use repair tools

### Repair Tools

Future tools will support:
- Page-level repair
- Index rebuilding
- Consistency checking
- Data extraction

---

## Related Documents

- [Architecture Overview](./ARCHITECTURE.md)
- [Pager Implementation](./PAGER_CONCURRENCY_COMPLETE.md)
- [WAL Implementation](./archive/WAL_IMPLEMENTATION.md)

---

**Last Updated**: 2026-05-10  
**Authors**: Hans W. Uhlig, Bob (AI Assistant)