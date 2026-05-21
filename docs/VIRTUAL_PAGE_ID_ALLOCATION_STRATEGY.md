# Virtual Page ID Allocation Strategy

## Question
Are virtual page numbers reused or monotonically increasing?

## Answer: Monotonically Increasing (No Reuse)

Virtual page IDs are **monotonically increasing** and are **NOT reused** after pages are freed.

## Implementation Details

### Allocation Mechanism
From [`PageMapper::allocate_virtual()`](../src/pager/page_mapper.rs:95):
```rust
pub fn allocate_virtual(&self) -> PageId {
    let mut next_id = self.next_virtual_id.write();
    let id = *next_id;
    *next_id = PageId::from(next_id.as_u64() + 1);  // Always increments
    *self.dirty.write() = true;
    id
}
```

The `next_virtual_id` counter:
- Starts at `PageId(2)` (after header and superblock)
- Always increments on allocation
- Never decrements or reuses freed IDs

### Cleanup Mechanism
While virtual IDs are not reused, the mapping table can be cleaned up:
```rust
pub fn unmap(&self, virtual_id: PageId) {
    let mut mapping = self.mapping.write();
    mapping.remove(&virtual_id);  // Remove mapping entry
    *self.dirty.write() = true;
}
```

This removes the mapping entry but does **not** reclaim the virtual ID for reuse.

## Design Rationale

### Why Monotonic (No Reuse)?

#### 1. **Massive ID Space**
- 64-bit PageId provides ~18.4 quintillion (2^64) possible IDs
- At 1 million allocations/second: 584,942 years to exhaust
- At 1 billion allocations/second: 584 years to exhaust
- Practical exhaustion is impossible for any real-world database

#### 2. **Simplicity**
- No need for virtual page free list
- No complex reuse logic
- Fewer edge cases and bugs
- Easier to reason about and debug

#### 3. **MVCC Compatibility**
- Old snapshots can safely reference old virtual IDs
- No risk of virtual ID reuse causing snapshot corruption
- Vacuum can clean up mappings for old versions safely

#### 4. **Debugging Benefits**
- Virtual IDs are unique over database lifetime
- Easier to trace page history
- No confusion from ID reuse

#### 5. **Identity Mapping Optimization**
- When `virtual_id == physical_id`, no mapping entry is stored
- This keeps the mapping table small for most pages
- Only remapped pages consume mapping table space

### What Gets Cleaned Up?

When a page is freed:
1. **Virtual ID is NOT reused** - Counter continues incrementing
2. **Mapping entry IS removed** - Saves memory and persistence space
3. **Physical page IS reused** - Goes back to free list

Example:
```
Initial state:
  virtual_id=100 → physical_id=50
  next_virtual_id=101

After freeing virtual page 100:
  mapping entry removed (saves space)
  next_virtual_id=101 (unchanged)
  
Next allocation:
  virtual_id=101 (new ID, not reusing 100)
  next_virtual_id=102
```

## Alternative Approach (Not Implemented)

### Virtual ID Reuse with Free List

**Would require:**
```rust
pub struct PageMapper {
    mapping: HashMap<PageId, PageId>,
    next_virtual_id: PageId,
    free_virtual_ids: Vec<PageId>,  // ← New: Track freed IDs
    dirty: bool,
}

pub fn allocate_virtual(&mut self) -> PageId {
    // Try to reuse freed ID first
    if let Some(id) = self.free_virtual_ids.pop() {
        return id;
    }
    // Otherwise allocate new
    let id = self.next_virtual_id;
    self.next_virtual_id = PageId(id.0 + 1);
    id
}

pub fn unmap(&mut self, virtual_id: PageId) {
    self.mapping.remove(&virtual_id);
    self.free_virtual_ids.push(virtual_id);  // ← Reclaim for reuse
}
```

**Why NOT implemented:**
- Adds complexity without practical benefit
- 64-bit ID space makes reuse unnecessary
- MVCC complications (old snapshots referencing reused IDs)
- More edge cases and potential bugs
- Minimal memory savings (only the counter, not the mappings)

## Memory Considerations

### Mapping Table Size
The mapping table only stores **non-identity mappings**:
- New pages: `virtual_id == physical_id` → No entry
- Remapped pages: `virtual_id != physical_id` → Entry stored
- Freed pages: Entry removed via `unmap()`

**Example:**
```
Database with 1 million pages:
- 900,000 pages never moved: 0 mapping entries (identity)
- 100,000 pages remapped: 100,000 entries × 16 bytes = 1.6 MB
- Total mapping overhead: ~1.6 MB

After vacuum (pages compacted):
- Many pages return to identity mapping
- Mapping table shrinks automatically
```

### Counter Overhead
The `next_virtual_id` counter:
- Size: 8 bytes (u64)
- Persisted in superblock
- Negligible overhead

## Vacuum Behavior

During vacuum, pages are moved to compact the file:

```rust
// Move physical page 1000 → 50
pager.move_physical_page(1000, 50)?;

// Update ALL virtual pages pointing to physical 1000
for virtual_id in mapper.find_virtual_pages_for_physical(1000) {
    mapper.remap(virtual_id, 50);
}

// If virtual_id == 50, the remap() will remove the mapping entry
// (returns to identity mapping)
```

After vacuum:
- Many pages return to identity mapping
- Mapping table shrinks
- Virtual IDs remain unchanged (stable references)

## Conclusion

**The monotonic allocation strategy is the correct design choice** because:

1. ✅ 64-bit ID space is effectively infinite
2. ✅ Simpler implementation with fewer bugs
3. ✅ MVCC-compatible (old snapshots safe)
4. ✅ Identity mapping keeps memory usage low
5. ✅ Vacuum can clean up mapping entries
6. ✅ Easier debugging and reasoning

**Virtual ID reuse would add complexity without practical benefit.**

## Future Considerations

If virtual ID exhaustion ever becomes a concern (extremely unlikely):
1. Add virtual ID free list
2. Implement reuse with MVCC safety checks
3. Add configuration option for reuse policy

However, with 64-bit IDs, this is not expected to be necessary in any realistic scenario.