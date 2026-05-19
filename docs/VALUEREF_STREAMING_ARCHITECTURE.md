# ValueRef-Based Streaming Architecture

## Overview

This document describes the ValueRef-based streaming architecture for handling large values in Nanostore. This architecture enables efficient storage and retrieval of values that exceed the inline threshold without loading entire values into memory.

## Motivation

### Problems with Full-Value Loading

1. **Memory Pressure**: Loading 100MB values into memory for every read/write operation
2. **Latency Spikes**: Large allocations cause GC pressure and unpredictable latency
3. **Throughput Limits**: Memory bandwidth becomes the bottleneck
4. **Concurrent Operations**: Multiple large value operations exhaust available memory

### Benefits of Streaming

1. **Constant Memory Usage**: Fixed-size buffers (8KB-64KB) regardless of value size
2. **Predictable Latency**: No large allocations or memory pressure
3. **Higher Throughput**: CPU and I/O can be pipelined effectively
4. **Better Concurrency**: Many operations can run simultaneously with minimal memory

## Architecture Components

### 1. Value Storage Strategy

Values are stored using one of three strategies based on size:

```
┌─────────────────────────────────────────────────────────────┐
│ Value Size Decision Tree                                     │
├─────────────────────────────────────────────────────────────┤
│                                                               │
│  Value Size < max_inline_size (e.g., 4KB)                   │
│  ├─> Store INLINE in table page                             │
│  │    • Direct storage in B-Tree leaf or LSM SSTable        │
│  │    • No indirection overhead                              │
│  │    • Fast access                                          │
│                                                               │
│  Value Size >= max_inline_size AND < 1 page                 │
│  ├─> Store as SINGLE_PAGE ValueRef                          │
│  │    • Allocate one overflow page                           │
│  │    • Store page ID in table                               │
│  │    • Single I/O operation                                 │
│                                                               │
│  Value Size >= 1 page                                        │
│  └─> Store as OVERFLOW_CHAIN ValueRef                       │
│       • Allocate linked chain of overflow pages              │
│       • Store first page ID in table                         │
│       • Stream across multiple pages                         │
│                                                               │
└─────────────────────────────────────────────────────────────┘
```

### 2. ValueRef Format

```rust
/// Reference to a value stored externally from the main table structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueRef {
    /// Value stored inline (no external reference needed)
    Inline,
    
    /// Value stored in a single overflow page
    SinglePage {
        page_id: PageId,
        offset: u16,
        length: u32,
    },
    
    /// Value stored across multiple linked overflow pages
    OverflowChain {
        first_page_id: PageId,
        total_length: u64,
        page_count: u32,
    },
}
```

### 3. Overflow Page Format

```
┌────────────────────────────────────────────────────────────┐
│ Overflow Page Header (32 bytes)                            │
├────────────────────────────────────────────────────────────┤
│ Magic: u32           (0x4F564C46 = "OVLF")                 │
│ Next Page ID: u32    (0 if last page)                      │
│ Data Length: u32     (bytes of data in this page)          │
│ Checksum: u32        (CRC32 of data)                       │
│ Reserved: [u8; 16]   (for future use)                      │
├────────────────────────────────────────────────────────────┤
│ Data (page_size - 32 bytes)                                │
│ ...                                                         │
└────────────────────────────────────────────────────────────┘
```

## Implementation Details

### BTree Integration

#### Storage Layout

```
B-Tree Leaf Node:
┌─────────────────────────────────────────────────────────┐
│ [Key1] [ValueRef1] [Key2] [ValueRef2] ...               │
└─────────────────────────────────────────────────────────┘
         │
         ├─> Inline: Value stored directly
         ├─> SinglePage: Points to overflow page
         └─> OverflowChain: Points to first page in chain
```

#### put_stream Implementation

```rust
impl MutableTable for BTreeWriter {
    fn put_stream(&mut self, key: &[u8], stream: &mut dyn ValueStream) 
        -> TableResult<u64> 
    {
        // 1. Check if value should be inline
        let size_hint = stream.size_hint();
        if let Some(size) = size_hint {
            if size <= self.max_inline_size() {
                // Read entire value and store inline
                let mut buffer = Vec::with_capacity(size);
                // ... read stream into buffer
                return self.put(key, &buffer);
            }
        }
        
        // 2. Allocate overflow pages and stream data
        let mut pages = Vec::new();
        let mut total_written = 0u64;
        let mut buffer = vec![0u8; PAGE_SIZE - OVERFLOW_HEADER_SIZE];
        
        loop {
            let n = stream.read(&mut buffer)?;
            if n == 0 { break; }
            
            let page_id = self.pager.allocate_page(PageType::Overflow)?;
            self.write_overflow_page(page_id, &buffer[..n], None)?;
            pages.push(page_id);
            total_written += n as u64;
        }
        
        // 3. Link pages together
        for i in 0..pages.len() - 1 {
            self.link_overflow_pages(pages[i], pages[i + 1])?;
        }
        
        // 4. Create ValueRef and store in B-Tree
        let value_ref = if pages.len() == 1 {
            ValueRef::SinglePage {
                page_id: pages[0],
                offset: 0,
                length: total_written as u32,
            }
        } else {
            ValueRef::OverflowChain {
                first_page_id: pages[0],
                total_length: total_written,
                page_count: pages.len() as u32,
            }
        };
        
        self.insert_value_ref(key, value_ref)?;
        Ok(total_written + key.len() as u64 + 16)
    }
}
```

#### get_stream Implementation

```rust
impl PointLookup for BTreeReader {
    fn get_stream(&self, key: &[u8], snapshot_lsn: LogSequenceNumber) 
        -> TableResult<Option<Box<dyn ValueStream + '_>>> 
    {
        // 1. Look up ValueRef in B-Tree
        let value_ref = self.lookup_value_ref(key, snapshot_lsn)?;
        
        match value_ref {
            None => Ok(None),
            
            Some(ValueRef::Inline) => {
                // Value is inline, use default implementation
                self.get(key, snapshot_lsn).map(|opt| {
                    opt.map(|buf| Box::new(SliceValueStream::new(buf.0)) 
                        as Box<dyn ValueStream>)
                })
            }
            
            Some(ValueRef::SinglePage { page_id, offset, length }) => {
                Ok(Some(Box::new(SinglePageStream {
                    pager: &self.pager,
                    page_id,
                    offset,
                    length,
                    position: 0,
                })))
            }
            
            Some(ValueRef::OverflowChain { first_page_id, total_length, .. }) => {
                Ok(Some(Box::new(OverflowChainStream {
                    pager: &self.pager,
                    current_page_id: first_page_id,
                    total_length,
                    position: 0,
                    buffer: Vec::new(),
                    buffer_pos: 0,
                })))
            }
        }
    }
}
```

### LSM Tree Integration

#### SSTable Format with ValueRefs

```
SSTable Data Block:
┌─────────────────────────────────────────────────────────┐
│ Entry 1: [Key Length][Key][ValueRef Type][ValueRef]    │
│ Entry 2: [Key Length][Key][ValueRef Type][ValueRef]    │
│ ...                                                      │
└─────────────────────────────────────────────────────────┘

ValueRef Encoding:
- Type byte: 0x00 = Inline, 0x01 = SinglePage, 0x02 = OverflowChain
- Inline: [length: u32][data: bytes]
- SinglePage: [page_id: u32][offset: u16][length: u32]
- OverflowChain: [first_page_id: u32][total_length: u64][page_count: u32]
```

#### Compaction with Streaming

During compaction, large values are streamed rather than loaded:

```rust
fn compact_with_streaming(
    &self,
    input_sstables: &[SStableId],
    output_level: usize,
) -> TableResult<Vec<SStableId>> {
    let mut merger = SStableMerger::new(input_sstables);
    let mut writer = SStableWriter::new(output_level);
    
    while let Some((key, value_ref)) = merger.next()? {
        match value_ref {
            ValueRef::Inline => {
                // Small value, copy directly
                writer.write_entry(key, value_ref)?;
            }
            
            ValueRef::SinglePage { .. } | ValueRef::OverflowChain { .. } => {
                // Large value, stream through
                let mut stream = self.get_value_stream(value_ref)?;
                writer.write_entry_stream(key, &mut stream)?;
            }
        }
    }
    
    Ok(writer.finish()?)
}
```

## Streaming API

### ValueStream Trait

```rust
pub trait ValueStream {
    /// Read data into the provided buffer.
    /// Returns the number of bytes read. A return value of 0 indicates EOF.
    fn read(&mut self, buf: &mut [u8]) -> TableResult<usize>;
    
    /// Get a size hint for the total value size, if known.
    fn size_hint(&self) -> Option<u64>;
}
```

### Common Implementations

#### SliceValueStream

For in-memory values:

```rust
pub struct SliceValueStream {
    data: Vec<u8>,
    position: usize,
}

impl ValueStream for SliceValueStream {
    fn read(&mut self, buf: &mut [u8]) -> TableResult<usize> {
        let remaining = self.data.len() - self.position;
        if remaining == 0 {
            return Ok(0);
        }
        
        let to_copy = remaining.min(buf.len());
        buf[..to_copy].copy_from_slice(
            &self.data[self.position..self.position + to_copy]
        );
        self.position += to_copy;
        Ok(to_copy)
    }
    
    fn size_hint(&self) -> Option<u64> {
        Some(self.data.len() as u64)
    }
}
```

#### OverflowChainStream

For paged values:

```rust
pub struct OverflowChainStream<'a, FS: FileSystem> {
    pager: &'a Pager<FS>,
    current_page_id: PageId,
    total_length: u64,
    position: u64,
    buffer: Vec<u8>,
    buffer_pos: usize,
}

impl<'a, FS: FileSystem> ValueStream for OverflowChainStream<'a, FS> {
    fn read(&mut self, buf: &mut [u8]) -> TableResult<usize> {
        if self.position >= self.total_length {
            return Ok(0);
        }
        
        let mut total_read = 0;
        
        while total_read < buf.len() && self.position < self.total_length {
            // Refill buffer if needed
            if self.buffer_pos >= self.buffer.len() {
                self.load_next_page()?;
            }
            
            // Copy from buffer
            let to_copy = (buf.len() - total_read)
                .min(self.buffer.len() - self.buffer_pos)
                .min((self.total_length - self.position) as usize);
            
            buf[total_read..total_read + to_copy].copy_from_slice(
                &self.buffer[self.buffer_pos..self.buffer_pos + to_copy]
            );
            
            total_read += to_copy;
            self.buffer_pos += to_copy;
            self.position += to_copy as u64;
        }
        
        Ok(total_read)
    }
    
    fn size_hint(&self) -> Option<u64> {
        Some(self.total_length)
    }
}
```

## Performance Characteristics

### Memory Usage

| Operation | Without Streaming | With Streaming |
|-----------|------------------|----------------|
| Write 100MB value | 100MB | 64KB buffer |
| Read 100MB value | 100MB | 64KB buffer |
| 10 concurrent 100MB ops | 1GB | 640KB |

### Throughput

Streaming enables better I/O pipelining:

```
Without Streaming:
[Allocate 100MB] -> [Read from source] -> [Write to disk] -> [Free 100MB]
                    ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
                    All in memory, sequential

With Streaming:
[Read 64KB] -> [Write 64KB] -> [Read 64KB] -> [Write 64KB] -> ...
               ^^^^^^^^^^^^    ^^^^^^^^^^^^
               Overlapped I/O possible
```

### Latency

- **Small values (< 4KB)**: No change, stored inline
- **Medium values (4KB - 1MB)**: Slight improvement due to reduced allocation
- **Large values (> 1MB)**: Significant improvement, no memory pressure

## Usage Examples

### Writing Large Values

```rust
use std::fs::File;
use std::io::Read;

// Stream from file
let mut file = File::open("large_data.bin")?;
let mut stream = FileValueStream::new(file);

let mut writer = table.writer(tx_id, snapshot_lsn)?;
writer.put_stream(b"large_key", &mut stream)?;
```

### Reading Large Values

```rust
// Stream to file
let mut output = File::create("output.bin")?;
let reader = table.reader(snapshot_lsn)?;

if let Some(mut stream) = reader.get_stream(b"large_key", snapshot_lsn)? {
    let mut buffer = vec![0u8; 65536]; // 64KB chunks
    loop {
        let n = stream.read(&mut buffer)?;
        if n == 0 { break; }
        output.write_all(&buffer[..n])?;
    }
}
```

### Streaming Network Data

```rust
struct NetworkValueStream {
    socket: TcpStream,
    total_size: u64,
    position: u64,
}

impl ValueStream for NetworkValueStream {
    fn read(&mut self, buf: &mut [u8]) -> TableResult<usize> {
        if self.position >= self.total_size {
            return Ok(0);
        }
        
        let to_read = buf.len().min((self.total_size - self.position) as usize);
        let n = self.socket.read(&mut buf[..to_read])
            .map_err(|e| TableError::Io(e))?;
        
        self.position += n as u64;
        Ok(n)
    }
    
    fn size_hint(&self) -> Option<u64> {
        Some(self.total_size)
    }
}
```

## Migration from BlobTable

The old `BlobTable` trait has been removed. Use the standard `MutableTable` and `PointLookup` traits instead:

### Before (BlobTable)

```rust
let mut blob_table: &mut dyn BlobTable = ...;
blob_table.put_blob(key, large_data)?;
let data = blob_table.get_blob(key)?;
```

### After (Streaming API)

```rust
let mut writer: &mut dyn MutableTable = ...;
let reader: &dyn PointLookup = ...;

// For small values, use put/get directly
writer.put(key, small_data)?;
let data = reader.get(key, snapshot_lsn)?;

// For large values, use streaming
let mut stream = create_stream(large_data);
writer.put_stream(key, &mut stream)?;

if let Some(mut stream) = reader.get_stream(key, snapshot_lsn)? {
    // Process stream in chunks
}
```

## Future Enhancements

### 1. Compression

Compress overflow pages individually:

```rust
ValueRef::OverflowChain {
    first_page_id: PageId,
    total_length: u64,        // Uncompressed size
    compressed_length: u64,   // Compressed size
    compression: CompressionKind,
    page_count: u32,
}
```

### 2. Deduplication

Share overflow chains for identical values:

```rust
ValueRef::Shared {
    ref_count_page: PageId,   // Page tracking reference count
    first_page_id: PageId,
    total_length: u64,
    content_hash: [u8; 32],   // SHA-256 for dedup
}
```

### 3. Tiered Storage

Move cold overflow chains to cheaper storage:

```rust
ValueRef::Tiered {
    tier: StorageTier,        // Hot, Warm, Cold, Archive
    location: TierLocation,   // Tier-specific location
    total_length: u64,
}
```

## See Also

- [Streaming API Implementation](STREAMING_API_IMPLEMENTATION.md)
- [Blob to Streaming Migration](BLOB_TO_STREAMING_MIGRATION.md)
- [ADR-012: Unified Table Architecture](adrs/012-unified-table-architecture.md)
- [File Format Specification](FILE_FORMAT.md)