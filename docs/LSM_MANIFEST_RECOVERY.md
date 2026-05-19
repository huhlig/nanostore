# LSM Manifest Recovery

## Overview

The LSM manifest recovery feature provides disaster recovery capabilities for the Nanostore database. If the manifest file becomes corrupted or lost, the system can reconstruct it by scanning all SSTable files on disk and reading their metadata.

## When to Use Recovery

Manifest recovery should be used in the following scenarios:

1. **Manifest Corruption**: The manifest file is corrupted and cannot be read
2. **Manifest Loss**: The manifest file is accidentally deleted or lost
3. **Database Migration**: Moving from an older format that didn't have a manifest
4. **Disaster Recovery**: Recovering from partial database corruption

## How It Works

### Recovery Algorithm

The recovery process follows these steps:

1. **Scan All Pages**: Iterate through all pages in the pager to find SSTable data pages (PageType::LsmData)

2. **Identify SSTables**: For each potential SSTable page:
   - Attempt to open it as an SSTable reader
   - Read the footer to extract metadata
   - Verify it's a first page (not a continuation page)
   - Collect unique SSTables (avoid duplicates)

3. **Assign to Levels**: Place all recovered SSTables in L0
   - Conservative approach: All SSTables go to L0 initially
   - L0 allows overlapping key ranges
   - Compaction will later move them to appropriate levels
   - Sorted by creation LSN (newest first) for efficient reads

4. **Build Version**: Construct a new manifest Version with:
   - All discovered SSTables in L0
   - Correct next_sstable_id (max ID + 1)
   - Empty higher levels (L1-L6)

5. **Write Manifest**: Persist the recovered manifest to disk

### Level Assignment Strategy

The recovery uses a **conservative placement strategy**:

- **All SSTables → L0**: Safest approach for disaster recovery
- **Why L0?**: 
  - L0 allows overlapping key ranges
  - No risk of data loss from incorrect level inference
  - Compaction will optimize placement later
- **Sorting**: SSTables sorted by creation LSN (newest first)
- **Alternative Rejected**: Trying to infer correct levels could lead to data loss

## API Usage

### Basic Recovery

```rust
use Nanostore::table::lsm::{Manifest, SStableConfig};
use Nanostore::pager::{Pager, PageId};
use std::sync::Arc;

// Open the pager
let pager = Arc::new(Pager::open(&fs, "database.db")?);

// Allocate or use existing manifest root page
let root_page_id = PageId::from(2);

// Configure SSTable settings
let config = SStableConfig::default();

// Recover the manifest
let version = Manifest::recover_from_sstables(
    Arc::clone(&pager),
    root_page_id,
    7,  // num_levels
    &config,
)?;

// Create new manifest with recovered version
let manifest = Manifest::new(pager, root_page_id, 7)?;
```

### Recovery with Existing Manifest

If you want to recover and compare with an existing manifest:

```rust
// Try to open existing manifest
let existing = Manifest::open(pager.clone(), root_page_id, 7);

// If it fails, recover from SSTables
let manifest = match existing {
    Ok(m) => m,
    Err(_) => {
        let version = Manifest::recover_from_sstables(
            pager.clone(),
            root_page_id,
            7,
            &config,
        )?;
        
        // Create new manifest with recovered data
        Manifest::new(pager, root_page_id, 7)?
    }
};
```

## Recovery Guarantees

### What is Recovered

✅ **Recovered**:
- All valid SSTable files
- SSTable metadata (ID, key ranges, sizes, LSNs)
- Correct next_sstable_id for future allocations
- File relationships (via key ranges)

### What is NOT Recovered

❌ **Not Recovered**:
- Original level assignments (all go to L0)
- Compaction history
- Deleted/obsolete SSTables (if already removed)
- Manifest edit history

### Data Safety

- **No Data Loss**: All valid SSTables are recovered
- **Conservative**: Places all SSTables in L0 to avoid incorrect assumptions
- **Corruption Handling**: Skips corrupted SSTable pages gracefully
- **Duplicate Prevention**: Ensures each SSTable is only included once

## Performance Considerations

### Recovery Time

Recovery time depends on:
- **Database Size**: Larger databases take longer to scan
- **Number of SSTables**: More SSTables = more metadata to read
- **Page Size**: Smaller pages = more pages to scan
- **Disk Speed**: I/O bound operation

Typical recovery times:
- Small DB (< 100 MB): < 1 second
- Medium DB (1-10 GB): 1-10 seconds
- Large DB (> 10 GB): 10+ seconds

### Memory Usage

Memory usage is proportional to:
- Number of SSTables (metadata stored in memory)
- Typical: ~200 bytes per SSTable
- Example: 10,000 SSTables ≈ 2 MB memory

### I/O Impact

- **Read-Only**: Recovery only reads, never writes (until final manifest write)
- **Sequential Scan**: Scans all pages sequentially
- **Cache Friendly**: Benefits from OS page cache

## Error Handling

### Recoverable Errors

The recovery process handles these errors gracefully:

1. **Corrupted SSTable Pages**: Skipped, valid SSTables still recovered
2. **Invalid Footers**: Page ignored, continues scanning
3. **Partial SSTables**: Only complete SSTables with valid footers recovered

### Non-Recoverable Errors

These errors will cause recovery to fail:

1. **Pager Errors**: Cannot read from database file
2. **No Valid SSTables**: Database appears empty or completely corrupted
3. **Memory Exhaustion**: Too many SSTables to fit in memory

## Testing

The recovery feature includes comprehensive tests:

- `test_manifest_recovery_empty_database`: Empty database recovery
- `test_manifest_recovery_single_sstable`: Single SSTable recovery
- `test_manifest_recovery_multiple_sstables_non_overlapping`: Multiple non-overlapping SSTables
- `test_manifest_recovery_overlapping_sstables`: Overlapping SSTables (all in L0)
- `test_manifest_recovery_mixed_overlapping`: Mix of overlapping and non-overlapping
- `test_manifest_recovery_preserves_sstable_ids`: Correct ID tracking
- `test_manifest_recovery_with_corrupted_sstable`: Graceful corruption handling

Run tests with:
```bash
cargo test --lib manifest::tests::test_manifest_recovery
```

## Best Practices

### When to Recover

1. **Automatic Recovery**: Consider automatic recovery on manifest open failure
2. **Manual Recovery**: Provide CLI tool for manual recovery operations
3. **Backup First**: Always backup database before recovery if possible

### After Recovery

1. **Verify Data**: Check that expected data is accessible
2. **Run Compaction**: Trigger compaction to optimize level placement
3. **Monitor Performance**: Watch for increased L0 read amplification
4. **Consider Checkpoint**: Create a checkpoint after successful recovery

### Prevention

1. **Regular Backups**: Backup manifest file regularly
2. **Checksums**: Enable checksums to detect corruption early
3. **Replication**: Use replication for high-availability scenarios
4. **Monitoring**: Monitor manifest file health

## Implementation Details

### Code Location

- **Implementation**: `src/table/lsm/manifest.rs`
- **Method**: `Manifest::recover_from_sstables()`
- **Helper**: `Manifest::assign_sstables_to_levels()`
- **Tests**: `src/table/lsm/manifest.rs` (tests module)

### Key Data Structures

```rust
pub struct SStableMetadata {
    pub id: SStableId,
    pub level: u32,
    pub min_key: Vec<u8>,
    pub max_key: Vec<u8>,
    pub num_entries: u64,
    pub total_size: u64,
    pub created_lsn: LogSequenceNumber,
    pub first_page_id: PageId,
    pub num_pages: u32,
    // ... additional fields
}
```

### Algorithm Complexity

- **Time**: O(n) where n = total pages in database
- **Space**: O(m) where m = number of SSTables
- **I/O**: Sequential scan of all pages

## Future Enhancements

Potential improvements for future versions:

1. **Parallel Scanning**: Use multiple threads to scan pages
2. **Incremental Recovery**: Resume from last known good state
3. **Smart Level Assignment**: Heuristics to infer original levels
4. **Recovery Validation**: Compare recovered manifest with backup
5. **Progress Reporting**: Callback for recovery progress updates

## See Also

- [LSM Tree Architecture](ARCHITECTURE.md)
- [File Format Specification](FILE_FORMAT.md)
- [Compaction Strategy](adrs/004-multiple-storage-engines.md)