# BTree Optimistic Concurrency Implementation Plan

## Issue: nanokv-f11r - BTree stress tests are slow (300s timeout)

## Current State Analysis

### Current Implementation (Latch Coupling)
The current `PagedBTree` uses **latch coupling** (crabbing):
- Acquires write latches while descending the tree
- Holds parent latches until child is determined "safe"
- Releases parent latches only when child won't split
- **Problem**: Under high concurrency with many splits, this causes significant root lock contention

### Key Code Locations
- `insert_internal()` (lines 1131-1255): Uses latch coupling
- `split_and_propagate()` (lines 1259-1293): Acquires root lock during splits
- `search_with_path()` (lines 608-660): Optimistic read path (no latches)

## Recommended Approach from Guide

The `safe_paged_btree_concurrency_guide.md` recommends **optimistic path validation**:

### Key Principles (from guide sections 8 & 16-18)

1. **Traverse without exclusive latches** - record path with versions
2. **Latch only when needed** - at leaf modification time
3. **Validate before publishing** - check parent version hasn't changed
4. **Retry on conflict** - restart from root if structure changed

### Algorithm Structure (from guide section 16)

```rust
pub fn insert(&self, key: Vec<u8>, value: Vec<u8>) -> Result<()> {
    loop {
        let path = self.find_leaf_path(&key)?;  // NO LATCHES
        let leaf_ref = path.leaf_ref();
        let leaf = self.cache.get(leaf_ref)?;

        if !leaf.page.owns_key(&key) {
            continue;  // Retry
        }

        if leaf.page.has_space_for(&key, &value) {
            // Simple case: no split needed
            let new_leaf = leaf.page.clone_with_insert(key, value)?;
            match self.publish_page_replacement(leaf_ref, new_leaf)? {
                PublishOutcome::Published => return Ok(()),
                PublishOutcome::Conflict => continue,  // Retry
            }
        }

        // Split case
        let split = leaf.page.clone_with_split_insert(key, value)?;
        match self.install_split(path, split)? {
            PublishOutcome::Published => return Ok(()),
            PublishOutcome::Conflict => continue,  // Retry
        }
    }
}
```

### Key Differences from Current Implementation

| Aspect | Current (Latch Coupling) | Recommended (Optimistic) |
|--------|-------------------------|--------------------------|
| **Traversal** | Acquires latches while descending | No latches during traversal |
| **Path tracking** | Records (parent, child) pairs | Records (page_id, version, child_index) |
| **Parent latch** | Held during child modification | Only acquired when split needed |
| **Conflict handling** | Blocks (waits for latch) | Detects and retries |
| **Root contention** | High (held during splits) | Low (only latched when validating) |

## Implementation Plan

### Phase 1: Add Page Versioning
**Goal**: Track page versions for conflict detection

```rust
// Add to BTreeNode or page header
struct PageVersion(u64);

// Increment on every write
fn write_node_with_version(&self, page_id: PageId, node: &BTreeNode) -> TableResult<PageVersion> {
    let new_version = self.get_next_version(page_id);
    // ... write node ...
    Ok(new_version)
}
```

### Phase 2: Optimistic Path Recording
**Goal**: Record path without holding latches

```rust
struct PathEntry {
    page_id: PageId,
    version: PageVersion,
    child_index: usize,  // Which child we descended to
}

fn find_leaf_path_optimistic(&self, key: &[u8]) -> TableResult<Vec<PathEntry>> {
    let mut path = Vec::new();
    let mut current = self.get_root_page_id();
    
    loop {
        // Read WITHOUT latch
        let (node, version) = self.read_node_with_version(current)?;
        
        match node {
            BTreeNode::Internal { entries, rightmost_child } => {
                let (child_id, child_idx) = find_child(&entries, rightmost_child, key);
                path.push(PathEntry {
                    page_id: current,
                    version,
                    child_index: child_idx,
                });
                current = child_id;
            }
            BTreeNode::Leaf { .. } => {
                path.push(PathEntry {
                    page_id: current,
                    version,
                    child_index: 0,
                });
                return Ok(path);
            }
        }
    }
}
```

### Phase 3: Optimistic Insert with Retry
**Goal**: Implement retry loop with conflict detection

```rust
fn insert_internal_optimistic(
    &self,
    key: Vec<u8>,
    value: Vec<u8>,
    tx_id: TransactionId,
    commit_lsn: LogSequenceNumber,
) -> TableResult<()> {
    loop {
        // Phase 1: Optimistic traversal (no latches)
        let path = self.find_leaf_path_optimistic(&key)?;
        let leaf_entry = path.last().unwrap();
        
        // Phase 2: Latch leaf and validate
        let leaf_latch = self.get_page_latch(leaf_entry.page_id);
        let _leaf_guard = leaf_latch.write();
        
        let (leaf_node, current_version) = self.read_node_with_version(leaf_entry.page_id)?;
        
        // Conflict check: has leaf changed since we read it?
        if current_version != leaf_entry.version {
            continue;  // Retry from root
        }
        
        // Phase 3: Modify leaf
        let modified_leaf = self.apply_insert_to_leaf(leaf_node, key.clone(), value.clone(), tx_id, commit_lsn)?;
        
        // Phase 4: Check if split needed
        if !modified_leaf.is_full() {
            // Simple case: just write the leaf
            self.write_node_with_version(leaf_entry.page_id, &modified_leaf)?;
            return Ok(());
        }
        
        // Phase 5: Split needed - validate parent and install
        match self.install_split_optimistic(path, modified_leaf, key, value)? {
            PublishOutcome::Published => return Ok(()),
            PublishOutcome::Conflict => continue,  // Retry
        }
    }
}
```

### Phase 4: Optimistic Split Installation
**Goal**: Validate parent before installing split

```rust
fn install_split_optimistic(
    &self,
    path: Vec<PathEntry>,
    leaf_node: BTreeNode,
    key: Vec<u8>,
    value: Vec<u8>,
) -> TableResult<PublishOutcome> {
    // Split the leaf
    let (right_page_id, median_key) = self.split_node_prepare(&leaf_node)?;
    
    // Check if this is root
    if path.len() == 1 {
        // Root split - need root lock
        return self.install_root_split(path[0].page_id, right_page_id, median_key);
    }
    
    // Get parent entry
    let parent_entry = &path[path.len() - 2];
    
    // Latch parent and validate
    let parent_latch = self.get_page_latch(parent_entry.page_id);
    let _parent_guard = parent_latch.write();
    
    let (parent_node, current_version) = self.read_node_with_version(parent_entry.page_id)?;
    
    // Conflict check: has parent changed?
    if current_version != parent_entry.version {
        return Ok(PublishOutcome::Conflict);
    }
    
    // Validate child pointer still correct
    if !self.validate_child_pointer(&parent_node, parent_entry.child_index, path.last().unwrap().page_id) {
        return Ok(PublishOutcome::Conflict);
    }
    
    // Install split atomically
    self.write_split_pages(path.last().unwrap().page_id, right_page_id, &median_key)?;
    self.update_parent_with_split(parent_entry.page_id, parent_node, median_key, right_page_id)?;
    
    Ok(PublishOutcome::Published)
}
```

## Performance Benefits

### Expected Improvements

1. **Reduced Root Contention**
   - Current: Root latched during entire descent + split
   - Optimistic: Root only latched when validating root split
   - **Impact**: 10-100x reduction in root lock hold time

2. **Better Concurrency**
   - Current: Blocks on every node in path
   - Optimistic: Only blocks at leaf modification
   - **Impact**: More concurrent operations possible

3. **Faster Non-Split Path**
   - Current: Acquires/releases multiple latches
   - Optimistic: Single latch at leaf
   - **Impact**: 2-5x faster for non-split inserts

### Trade-offs

1. **Retries**: Some operations will retry on conflicts
   - Acceptable: Conflicts are rare in practice
   - Guide: "Conflict is not failure. Conflict means: someone else modified the structure; restart from root"

2. **Complexity**: More complex implementation
   - Mitigated: Guide provides clear pseudocode
   - Worth it: Significant performance gains

## Testing Strategy

### Unit Tests
- Test optimistic path recording
- Test version conflict detection
- Test retry logic

### Concurrency Tests
- Run existing stress tests with optimistic implementation
- Measure improvement in test completion time
- Verify no correctness regressions

### Performance Benchmarks
- Measure insert throughput under high concurrency
- Compare latch coupling vs optimistic
- Track retry rates

## Success Criteria

1. **Correctness**: All existing tests pass
2. **Performance**: Stress tests complete in < 60s (vs current 300s)
3. **Concurrency**: 95%+ success rate maintained
4. **Retry Rate**: < 5% of operations retry

## References

- `safe_paged_btree_concurrency_guide.md` - Sections 6, 8, 16-18
- `docs/BTREE_CONCURRENCY_ISSUES.md` - Current problem analysis
- Issue `nanokv-f11r` - Performance issue tracking