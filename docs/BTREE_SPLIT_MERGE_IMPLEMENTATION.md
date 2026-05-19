# B-Tree Split and Merge Implementation

## Overview

This document describes the implementation of B-Tree node split and merge operations for the paged B-Tree table engine in Nanostore.

## Implementation Date

2026-05-09

## Components Implemented

### 1. Node Split Operation (`split_node`)

**Location**: `src/table/btree/paged.rs`

**Purpose**: Splits a full B-Tree node into two nodes when it exceeds capacity (DEFAULT_ORDER = 64 keys).

**Algorithm**:
- For internal nodes:
  - Splits entries at midpoint
  - Left node keeps entries [0..mid]
  - Right node gets entries [mid+1..]
  - Median key is promoted to parent
  - Properly updates child pointers

- For leaf nodes:
  - Splits entries at midpoint
  - Left node keeps entries [0..mid]
  - Right node gets entries [mid..]
  - Updates next_leaf pointers for sequential scans
  - Median key is promoted to parent

**Returns**: `(right_page_id, median_key)` tuple

### 2. Node Merge Operation (`merge_nodes`)

**Location**: `src/table/btree/paged.rs`

**Purpose**: Merges two adjacent nodes when combined size is below threshold.

**Algorithm**:
- Validates nodes are same type (both internal or both leaf)
- Checks if merge is possible (combined size ≤ DEFAULT_ORDER)
- For internal nodes:
  - Adds separator key from parent
  - Combines all entries
  - Updates child pointers
- For leaf nodes:
  - Combines all entries
  - Updates next_leaf pointer
- Frees the right node page

**Returns**: `bool` indicating success

### 3. Key Redistribution Operation (`redistribute_keys`)

**Location**: `src/table/btree/paged.rs`

**Purpose**: Balances keys between two adjacent nodes without merging.

**Algorithm**:
- Calculates target distribution (total_keys / 2)
- Moves keys from fuller node to emptier node
- For internal nodes:
  - Handles separator key from parent
  - Updates child pointers correctly
  - Returns new separator key
- For leaf nodes:
  - Moves entries directly
  - Returns new separator key (first key of right node)

**Returns**: `Option<Vec<u8>>` with new separator key

### 4. Insert with Split (`insert_internal`)

**Location**: `src/table/btree/paged.rs`

**Purpose**: Inserts key-value pairs with automatic node splitting.

**Features**:
- MVCC support through version chains
- Automatic version commit (simplified for testing)
- Triggers split when node becomes full
- Updates existing keys by prepending to version chain

### 5. Delete with Merge (`delete_internal`)

**Location**: `src/table/btree/paged.rs`

**Purpose**: Deletes keys with automatic node rebalancing.

**Features**:
- MVCC support (marks as deleted in version chain)
- Checks for underflow after deletion
- Placeholder for rebalancing logic

### 6. Flush Implementation

**Location**: `src/table/btree/paged.rs`

**Purpose**: Applies pending changes from writer to B-Tree.

**Implementation**:
- Processes all pending inserts and deletes
- Calls `insert_internal` or `delete_internal` for each operation
- Handles splits and merges automatically

## Known Limitations

### 1. Root Pointer Update

**Issue**: The `root_page_id` field in `PagedBTree` is immutable, preventing root node splits from updating the root pointer.

**Impact**: 
- Root splits create a new root but can't update the pointer
- Tree becomes inaccessible after root split
- Tests fail when tree grows beyond single root node capacity

**Solution Required**:
- Make `root_page_id` mutable (requires `Arc<RwLock<PageId>>` or similar)
- Persist root pointer in superblock/metadata
- Update all read operations to use current root

**Tracking**: Issue Nanostore-??? (to be created)

### 2. Parent Pointer Traversal

**Issue**: Current implementation doesn't maintain parent pointers or path stack during traversal.

**Impact**:
- Can't propagate splits up the tree beyond root
- Can't find siblings for merge/redistribute operations
- Rebalancing after delete is not fully implemented

**Solution Required**:
- Add path stack to track traversal
- Or maintain parent pointers in nodes
- Implement full split propagation
- Implement sibling finding for rebalancing

### 3. Simplified MVCC

**Issue**: Versions are committed immediately rather than at transaction commit time.

**Impact**:
- Not true MVCC behavior
- Uncommitted changes are visible
- Suitable for testing but not production

**Solution Required**:
- Defer commit until transaction commit
- Track uncommitted versions
- Implement proper visibility rules

## Test Coverage

**Location**: `tests/btree_split_merge_tests.rs`

**Tests Implemented**:
1. `test_btree_node_split_on_insert` - Tests split on large insert
2. `test_btree_sequential_inserts` - Tests sequential key insertion
3. `test_btree_reverse_inserts` - Tests reverse order insertion
4. `test_btree_random_inserts` - Tests pseudo-random insertion
5. `test_btree_update_existing_keys` - Tests key updates with MVCC
6. `test_btree_delete_keys` - Tests deletion with MVCC
7. `test_btree_mixed_operations` - Tests mixed insert/update/delete

**Test Status**:
- 1 passing (update_existing_keys)
- 6 failing due to root pointer limitation

## Code Quality

### Strengths
- Clean separation of concerns
- Well-documented functions
- Proper error handling
- MVCC-aware implementation

### Areas for Improvement
- Root pointer mutability
- Parent pointer tracking
- Full split propagation
- Complete rebalancing logic

## Performance Considerations

### Split Operation
- Time: O(n) where n = node size
- Space: O(n) for new node allocation
- I/O: 2 page writes (left and right nodes)

### Merge Operation
- Time: O(n) where n = combined node size
- Space: O(1) (reuses left node)
- I/O: 1 page write + 1 page free

### Redistribute Operation
- Time: O(n) where n = keys moved
- Space: O(n) for temporary storage
- I/O: 2 page writes (both nodes)

## Future Work

1. **Fix Root Pointer** (High Priority)
   - Implement mutable root pointer
   - Persist in superblock
   - Update all access paths

2. **Complete Rebalancing** (High Priority)
   - Implement sibling finding
   - Complete merge/redistribute after delete
   - Add parent pointer tracking

3. **Optimize Splits** (Medium Priority)
   - Implement bulk loading
   - Add split prediction
   - Optimize for sequential inserts

4. **Add Metrics** (Low Priority)
   - Track split/merge counts
   - Monitor node utilization
   - Measure rebalancing effectiveness

## References

- B-Tree algorithm: Cormen et al., "Introduction to Algorithms"
- MVCC design: PostgreSQL documentation
- Pager integration: `docs/PAGER_CONCURRENCY_IMPROVEMENT.md`

## Made with Bob

This implementation was created with assistance from Bob, an AI coding assistant.