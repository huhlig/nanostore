# Virtual vs Physical Page IDs: Two Separate Concepts

## The Key Distinction

There are **TWO separate ID spaces** in the virtual-to-physical page mapping design:

### 1. Virtual Page IDs (Logical Layer)
- **Used by**: Tables (B-trees, LSM, indexes, etc.)
- **Allocation**: Monotonically increasing, never reused
- **Purpose**: Stable references that don't change when pages move
- **Managed by**: [`PageMapper`](../src/pager/page_mapper.rs)

### 2. Physical Page IDs (Storage Layer)
- **Used by**: File I/O operations
- **Allocation**: Reused via free list
- **Purpose**: Actual file positions for reading/writing
- **Managed by**: [`FreeList`](../src/pager/freelist.rs)

## Why We Need BOTH

### Virtual IDs: Stable References
```rust
// Table stores virtual page IDs
struct BTreeNode {
    children: Vec<PageId>,  // ← Virtual IDs (never change)
}

// When vacuum moves a page:
// - Virtual ID stays the same (table doesn't need updates)
// - Physical ID changes (file position changes)
// - PageMapper updates the mapping
```

### Physical IDs: Efficient Storage
```rust
// Physical pages are reused to avoid file growth
// When a page is freed:
// 1. Virtual ID is NOT reused (monotonic)
// 2. Physical ID goes to free list (reused)
// 3. Next allocation gets physical ID from free list
```

## Complete Lifecycle Example

### Scenario: Allocate, Free, Vacuum

```rust
// === Initial State ===
Virtual IDs:  [2, 3, 4, 5, ...]  (monotonic counter)
Physical IDs: [2, 3, 4, 5, ...]  (file positions)
Free List:    []
Mapping:      {} (all identity: virtual == physical)

// === Step 1: Allocate new page ===
let virtual_id = page_mapper.allocate_virtual();  // Returns 6
let physical_id = pager.allocate_physical();      // Returns 6 (from file growth)
// Mapping: {} (still identity: 6 → 6)

// === Step 2: Free page 4 ===
page_mapper.unmap(PageId(4));           // Remove mapping entry
pager.free_physical(PageId(4));         // Add to free list

Virtual IDs:  [2, 3, 4, 5, 6, ...]  (4 is NOT reused)
Physical IDs: [2, 3, _, 5, 6, ...]  (4 is free)
Free List:    [4]
Mapping:      {} (4's mapping removed)

// === Step 3: Allocate another page ===
let virtual_id = page_mapper.allocate_virtual();  // Returns 7 (new virtual ID)
let physical_id = pager.allocate_physical();      // Returns 4 (from free list!)
page_mapper.remap(PageId(7), PageId(4));          // Map virtual 7 → physical 4

Virtual IDs:  [2, 3, 4, 5, 6, 7, ...]  (7 is new)
Physical IDs: [2, 3, 4, 5, 6, ...]     (4 is reused)
Free List:    []
Mapping:      {7 → 4}  (non-identity mapping)

// === Step 4: Vacuum moves page 6 to position 4 ===
// (Assume page 7 was freed, so physical 4 is free again)
pager.move_physical_page(6, 4);         // Copy data from position 6 to 4
page_mapper.remap(PageId(6), PageId(4)); // Update mapping: 6 → 4
pager.free_physical(PageId(6));         // Add position 6 to free list

Virtual IDs:  [2, 3, 4, 5, 6, 7, ...]  (unchanged)
Physical IDs: [2, 3, 4, 5, _, ...]     (6 is now free)
Free List:    [6]
Mapping:      {6 → 4}  (6 now points to physical 4)
```

## The Free List is ESSENTIAL

**Yes, we absolutely need the free list!** Here's why:

### Without Free List (File Would Grow Forever)
```
Allocate page → File grows to position 100
Free page 50  → Position 50 is wasted
Allocate page → File grows to position 101 (can't reuse 50)
Free page 60  → Position 60 is wasted
...
Result: File full of holes, grows indefinitely
```

### With Free List (Efficient Storage)
```
Allocate page → File grows to position 100
Free page 50  → Add physical 50 to free list
Allocate page → Reuse physical 50 from free list (no file growth!)
Free page 60  → Add physical 60 to free list
...
Result: File stays compact, physical pages reused
```

## What `unmap()` Does vs Free List

### `PageMapper::unmap()` - Removes Mapping Entry
```rust
pub fn unmap(&self, virtual_id: PageId) {
    let mut mapping = self.mapping.write();
    mapping.remove(&virtual_id);  // ← Remove from HashMap
    *self.dirty.write() = true;
}
```

**Effect:**
- Removes the virtual → physical mapping entry
- Saves memory (HashMap entry removed)
- Virtual ID is NOT reclaimed for reuse
- Does NOT affect the free list

### `FreeList::add()` - Reclaims Physical Page
```rust
pub fn add(&self, physical_id: PageId) {
    self.free_pages.push(physical_id);  // ← Add to free list
    counter!("pager.freelist.pages_freed").increment(1);
}
```

**Effect:**
- Adds physical page ID to free list
- Physical page can be reused for new allocations
- Prevents file growth
- Does NOT affect virtual IDs

## Complete Page Deallocation Flow

When a table frees a page, BOTH operations must happen:

```rust
impl Pager {
    pub fn free_page(&mut self, virtual_id: PageId) -> PagerResult<()> {
        // 1. Get physical page ID
        let physical_id = self.page_mapper.translate(virtual_id);
        
        // 2. Remove virtual mapping (saves memory)
        self.page_mapper.unmap(virtual_id);
        
        // 3. Add physical page to free list (enables reuse)
        self.free_list.add(physical_id);
        
        Ok(())
    }
}
```

**Both steps are necessary:**
- `unmap()` - Cleans up mapping table
- `free_list.add()` - Enables physical page reuse

## Vacuum Behavior

During vacuum, the free list is used to compact the file:

```rust
pub fn vacuum_pager(&mut self) -> PagerResult<VacuumStats> {
    // 1. Find highest used physical page
    let max_physical = self.find_max_physical_page();
    
    // 2. Get free physical pages below max (from free list)
    let free_pages = self.free_list.get_pages_below(max_physical);
    
    // 3. Move high pages to low free positions
    for high_physical in (0..=max_physical).rev() {
        if self.is_physical_page_in_use(high_physical) {
            if let Some(low_physical) = free_pages.pop_front() {
                // Move physical page
                self.move_physical_page(high_physical, low_physical)?;
                
                // Update ALL virtual pages pointing to high_physical
                for virtual_id in self.page_mapper.find_virtual_pages_for_physical(high_physical) {
                    self.page_mapper.remap(virtual_id, low_physical);
                }
                
                // high_physical is now free
                self.free_list.add(high_physical);
            }
        }
    }
    
    // 4. Truncate file (remove high free pages)
    self.truncate_file()?;
    
    // 5. Free list now only contains pages below new file size
    Ok(stats)
}
```

**After vacuum:**
- Virtual IDs unchanged (tables don't need updates)
- Physical pages compacted (file shrinks)
- Free list contains only low-numbered pages
- Mapping table updated with new physical locations

## Memory Overhead Comparison

### Virtual ID Counter
- Size: 8 bytes (u64)
- Growth: Never shrinks (monotonic)
- Impact: Negligible

### Mapping Table (HashMap)
- Size: ~16 bytes per non-identity mapping
- Growth: Only for remapped pages
- Shrinks: When pages freed (`unmap()`)
- Impact: Moderate (only remapped pages)

### Free List
- Size: ~8 bytes per free physical page
- Growth: When pages freed
- Shrinks: When pages allocated
- Impact: Moderate (only free pages)

**Example with 1M pages:**
```
Virtual counter:     8 bytes
Mapping table:       ~1.6 MB (100k remapped pages)
Free list:           ~80 KB (10k free pages)
Total overhead:      ~1.7 MB
```

## Summary

| Aspect | Virtual IDs | Physical IDs |
|--------|-------------|--------------|
| **Purpose** | Stable references | File positions |
| **Allocation** | Monotonic (never reused) | Reused via free list |
| **Managed by** | PageMapper | FreeList |
| **When freed** | `unmap()` removes mapping | Added to free list |
| **Growth** | Counter always increases | File can shrink via vacuum |
| **Used by** | Tables, indexes | File I/O operations |

**Key Insight:** Virtual IDs provide stability (references don't change), while physical IDs provide efficiency (storage is reused). Both are essential for the design to work correctly.

## Why This Design Works

1. **Tables use virtual IDs** → References never need updating
2. **Physical IDs are reused** → File doesn't grow unnecessarily
3. **Vacuum moves physical pages** → File can be compacted
4. **Mapping translates virtual → physical** → Everything stays consistent
5. **Free list enables reuse** → Storage is efficient

Without the free list, physical pages couldn't be reused, and the file would grow indefinitely with holes. The free list is absolutely essential for efficient storage management.