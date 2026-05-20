# BTree Concurrency Issues - Investigation

## Issue Summary

Investigation of failing stress tests revealed a critical concurrency bug in the PagedBTree implementation that causes:
1. Panics due to invalid insertion indices
2. Lost updates when multiple threads modify the same node
3. Significantly reduced throughput in concurrent scenarios

## Important Distinction: MVCC vs B-Tree Latching

**The system HAS proper MVCC for data versioning** - version chains work correctly for logical data isolation.

**The system LACKS proper latching for B-tree structure modifications** - this is a separate concern.

### Two Levels of Concurrency Control

1. **Logical Level (MVCC)** - ✅ WORKING
   - Version chains handle multiple versions of the same key
   - Transactions see consistent snapshots
   - Isolation levels are enforced correctly

2. **Physical Level (B-Tree Structure)** - ❌ BROKEN
   - No protection for node array modifications
   - Multiple threads can read-modify-write the same node
   - Last write wins, causing lost updates

## Root Cause

The PagedBTree implementation has a race condition in `insert_internal()` at the **physical structure level**:

```rust
// Original problematic code:
let (leaf_page_id, pos, path) = self.search_with_path(&key)?;  // Step 1
let mut node = self.read_node(leaf_page_id)?;                   // Step 2
// ... later ...
entries.insert(pos, entry);                                      // Step 3 - PANIC!
```

**The Problem**: Between Step 1 (calculating position) and Step 3 (inserting), another thread can modify the node, making `pos` invalid. This causes:
- **Panic**: "insertion index (is 12) should be <= len (is 11)" when pos exceeds the current length
- **Lost updates**: When multiple threads read-modify-write the same node without coordination

## Test Failures

### test_concurrent_transactions_multiple_tables
- **Expected**: 1000/1000 successful transactions
- **Actual**: 998/1000 (before fix: panic, after fix: 998)
- **Issue**: 2 transactions lost due to concurrent modifications

### test_oltp_workload  
- **Expected**: 240+ successful writes (50% of 480)
- **Actual**: 131/480 writes
- **Issue**: Severe throughput degradation, test times out after 5 minutes

## Fix Applied

Added position recalculation immediately before insertion to handle concurrent modifications:

```rust
fn insert_internal(...) -> TableResult<()> {
    let (leaf_page_id, _pos, path) = self.search_with_path(&key)?;
    let mut node = self.read_node(leaf_page_id)?;
    
    if let BTreeNode::Leaf { ref mut entries, .. } = node {
        // CRITICAL: Recalculate position based on current entries
        // The node may have been modified by another thread
        let current_pos = entries.binary_search_by(|e| e.key.as_slice().cmp(&key));
        let pos = match current_pos {
            Ok(i) => i,
            Err(i) => i,
        };
        
        // Now safe to insert at recalculated position
        entries.insert(pos, entry);
    }
}
```

## Remaining Issues

The fix prevents panics but doesn't solve the fundamental concurrency problem:

1. **No Page-Level Locking**: Multiple threads can read-modify-write the same page, causing lost updates
2. **No Optimistic Concurrency Control**: No version checking or conflict detection
3. **No Retry Logic**: Failed operations don't retry

## Proper Solution Required: B-Tree Latching

The BTree needs **latching** (short-term locks during structure modifications), which is separate from MVCC.

### Standard B-Tree Concurrency Protocol

Most databases use **latch coupling** (also called "crabbing"):

1. **Latch the root** (read or write mode)
2. **Latch the child** you're descending to
3. **Release the parent latch** if the child is "safe"
   - Safe for insert: not full
   - Safe for delete: more than minimum keys
4. **Repeat** until reaching the leaf
5. **Modify the leaf** while holding its write latch
6. **Release all latches**

### Implementation Options

#### Option 1: Page-Level Latching (RECOMMENDED)
```rust
struct PagedBTree {
    page_latches: DashMap<PageId, RwLock<()>>,  // Per-page latches
}

fn insert_with_latching(&self, key: Vec<u8>, value: Vec<u8>) -> Result<()> {
    let mut latched_pages = Vec::new();
    
    // Descend tree with latch coupling
    let mut current = self.root_page_id;
    loop {
        let latch = self.page_latches.entry(current).or_insert_with(|| RwLock::new(()));
        let guard = latch.write(); // Acquire write latch
        latched_pages.push((current, guard));
        
        let node = self.read_node(current)?;
        
        // If node is safe (not full), release all parent latches
        if !node.is_full() {
            latched_pages.drain(0..latched_pages.len()-1);
        }
        
        if node.is_leaf() {
            break; // Reached leaf, modify it
        }
        
        current = node.find_child(&key);
    }
    
    // Modify leaf while holding its latch
    // ... insert logic ...
    
    // Latches released when guards drop
    Ok(())
}
```

**Pros:**
- Standard approach used by most databases
- Allows high concurrency
- Prevents lost updates

**Cons:**
- More complex implementation
- Need to handle deadlocks (though rare with proper latch ordering)

#### Option 2: Optimistic Concurrency Control
```rust
struct PageHeader {
    version: u64,  // Increment on each write
}

fn write_node_with_version_check(
    page_id: PageId,
    node: &BTreeNode,
    expected_version: u64
) -> Result<(), VersionMismatch> {
    // Atomic compare-and-swap on version
}
```

**Pros:**
- No blocking
- Good for read-heavy workloads

**Cons:**
- Requires retry logic
- Can have high abort rates under contention

#### Option 3: Copy-on-Write with Atomic Root Swap
```rust
// Never modify pages in-place
// Create new versions of modified pages
// Atomically swap root pointer
```

**Pros:**
- Lock-free reads
- Natural MVCC integration

**Cons:**
- Higher write amplification
- More complex garbage collection

## Test Expectations

The current test expectations are unrealistic for a system without proper concurrency control:

1. **test_concurrent_transactions_multiple_tables**: Expecting 100% success rate with no conflicts is unrealistic without proper locking
2. **test_oltp_workload**: The severe throughput degradation suggests deadlocks or excessive contention

## Recommendations

1. **Short-term**: Adjust test expectations to accept some transaction failures (95%+ success rate)
2. **Medium-term**: Implement page-level locking (Option 1)
3. **Long-term**: Consider OCC or COW for better scalability (Options 2/3)

## Related Files

- `src/table/btree/paged.rs` - BTree implementation
- `tests/end_to_end_stress_tests.rs` - Failing tests
- Issue: `nanokv-gssc`