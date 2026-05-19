# MVCC Test Coverage Analysis

## Overview

This document analyzes the current MVCC (Multi-Version Concurrency Control) test coverage in Nanostore and identifies gaps that need to be filled per issue Nanostore-xzt.

## Current Test Coverage

### 1. Snapshot Visibility Tests (`tests/snapshot_visibility_tests.rs`)

**Covered:**
- ✅ Basic visibility with watermark optimization
- ✅ Visibility with empty active transaction list
- ✅ Visibility with single active transaction
- ✅ Visibility with many concurrent transactions (1000+)
- ✅ Performance benchmarks for visibility checking
- ✅ Sparse transaction IDs
- ✅ LSN boundary conditions
- ✅ Watermark computation

**Gaps:**
- ❌ No tests using actual Database/Transaction APIs
- ❌ No cross-engine visibility tests
- ❌ No tests with real data operations

### 2. Conflict Detection Tests (`tests/conflict_detector_tests.rs`)

**Covered:**
- ✅ Write-write conflicts on same key
- ✅ No conflict on different keys
- ✅ Same transaction can relock
- ✅ Lock release and reacquisition
- ✅ Different tables no conflict
- ✅ Read-write conflict detection
- ✅ Read-write no conflict on different keys

**Gaps:**
- ❌ No serialization conflict tests
- ❌ No deadlock detection tests
- ❌ No range lock conflict tests

### 3. Engine-Specific MVCC Tests

#### Graph MVCC Tests (`tests/graph_mvcc_tests.rs`)
**Covered:**
- ✅ Uncommitted edges not visible
- ✅ Committed edges visible
- ✅ Edge removal creates tombstone
- ✅ Vacuum removes old versions

**Gaps:**
- ❌ No concurrent transaction tests
- ❌ No snapshot isolation tests
- ❌ No long-running transaction tests

#### TimeSeries MVCC Tests (`tests/timeseries_mvcc_tests.rs`)
**Covered:**
- ✅ Snapshot isolation with multiple versions
- ✅ Multiple versions at same timestamp
- ✅ Vacuum with active snapshots

**Gaps:**
- ❌ No write-write conflict tests
- ❌ No read-committed vs snapshot isolation comparison
- ❌ No cross-series transaction tests

#### Paged BTree MVCC Tests (`tests/paged_btree_comprehensive_tests.rs`)
**Covered:**
- ✅ Snapshot isolation
- ✅ Concurrent readers
- ✅ Reader-writer isolation

**Gaps:**
- ❌ No write-write conflict tests
- ❌ No long-running transaction tests

### 4. Transaction Integration Tests (`tests/transaction_integration_tests.rs`)

**Covered:**
- ✅ Transaction creation and state
- ✅ Put/get/delete operations
- ✅ Isolation level configuration
- ✅ Basic commit/rollback

**Gaps:**
- ❌ No actual isolation level behavior tests
- ❌ No cross-table transaction tests
- ❌ No long-running transaction tests

### 5. Database API Tests (`tests/database_api_tests.rs`)

**Covered:**
- ✅ Table management (create, drop, list)
- ✅ Basic CRUD operations

**Gaps:**
- ❌ No transaction-level tests
- ❌ No snapshot management tests
- ❌ No MVCC visibility tests

## Required Test Coverage (per Nanostore-xzt)

### 1. Basic Snapshot Isolation (Concurrent Reads See Consistent State)

**Status:** Partially covered
**Gaps:**
- Need tests across all storage engines (BTree, Hash, ART, LSM, Graph, TimeSeries)
- Need tests with Database API (not just low-level)
- Need tests with multiple concurrent readers

**Recommended Tests:**
```rust
test_snapshot_isolation_btree_concurrent_reads()
test_snapshot_isolation_hash_concurrent_reads()
test_snapshot_isolation_art_concurrent_reads()
test_snapshot_isolation_lsm_concurrent_reads()
test_snapshot_isolation_graph_concurrent_reads()
test_snapshot_isolation_timeseries_concurrent_reads()
```

### 2. Write-Write Conflicts

**Status:** Partially covered (conflict detector only)
**Gaps:**
- Need end-to-end tests with actual transactions
- Need tests across all engines
- Need tests with Database API

**Recommended Tests:**
```rust
test_write_write_conflict_btree()
test_write_write_conflict_hash()
test_write_write_conflict_art()
test_write_write_conflict_lsm()
test_write_write_conflict_graph()
test_write_write_conflict_timeseries()
test_write_write_conflict_cross_table()
```

### 3. Read-Committed vs Snapshot Isolation

**Status:** Not covered
**Gaps:**
- No tests comparing isolation levels
- No tests showing different behavior

**Recommended Tests:**
```rust
test_read_committed_sees_concurrent_commits()
test_snapshot_isolation_does_not_see_concurrent_commits()
test_isolation_level_comparison()
```

### 4. Long-Running Transactions

**Status:** Not covered
**Gaps:**
- No tests with transactions spanning multiple operations
- No tests with version chain growth
- No tests with vacuum during long transactions

**Recommended Tests:**
```rust
test_long_running_transaction_visibility()
test_long_running_transaction_blocks_vacuum()
test_long_running_transaction_version_chain_growth()
test_long_running_transaction_with_concurrent_writes()
```

### 5. Vacuum with Active Snapshots

**Status:** Partially covered (timeseries only)
**Gaps:**
- Need tests across all engines
- Need tests with multiple active snapshots
- Need tests verifying vacuum respects watermark

**Recommended Tests:**
```rust
test_vacuum_respects_active_snapshot_btree()
test_vacuum_respects_active_snapshot_hash()
test_vacuum_respects_active_snapshot_art()
test_vacuum_respects_active_snapshot_lsm()
test_vacuum_respects_active_snapshot_graph()
test_vacuum_with_multiple_active_snapshots()
test_vacuum_watermark_computation()
```

### 6. Cross-Table Transaction Visibility

**Status:** Not covered
**Gaps:**
- No tests with transactions spanning multiple tables
- No tests verifying atomicity across tables
- No tests with cross-table conflicts

**Recommended Tests:**
```rust
test_cross_table_transaction_atomicity()
test_cross_table_snapshot_isolation()
test_cross_table_write_write_conflict()
test_cross_table_rollback()
test_cross_table_commit()
```

## Test Implementation Plan

### Phase 1: Core Snapshot Isolation Tests
1. Create `tests/mvcc_snapshot_isolation_tests.rs`
2. Implement snapshot isolation tests for each engine
3. Test concurrent readers see consistent state
4. Test snapshots don't see uncommitted changes

### Phase 2: Conflict Detection Tests
1. Extend `tests/conflict_detector_tests.rs` or create new file
2. Implement end-to-end write-write conflict tests
3. Test conflicts across all engines
4. Test conflict resolution and error handling

### Phase 3: Isolation Level Tests
1. Create `tests/mvcc_isolation_levels_tests.rs`
2. Implement read-committed tests
3. Implement snapshot isolation tests
4. Compare behavior between isolation levels

### Phase 4: Long-Running Transaction Tests
1. Create `tests/mvcc_long_running_tests.rs`
2. Test version chain growth
3. Test vacuum interaction
4. Test concurrent operations

### Phase 5: Vacuum Integration Tests
1. Extend existing vacuum tests
2. Test vacuum with active snapshots across all engines
3. Test watermark computation
4. Test vacuum doesn't remove visible versions

### Phase 6: Cross-Table Tests
1. Create `tests/mvcc_cross_table_tests.rs`
2. Test atomicity across tables
3. Test snapshot isolation across tables
4. Test conflicts across tables

## Success Criteria

- [ ] All 6 test categories have comprehensive coverage
- [ ] Tests cover all storage engines (BTree, Hash, ART, LSM, Graph, TimeSeries)
- [ ] Tests use Database API (not just low-level APIs)
- [ ] Tests verify MVCC guarantees (isolation, atomicity, consistency)
- [ ] Tests verify vacuum correctness with active snapshots
- [ ] All tests pass consistently
- [ ] Test coverage documented

## Estimated Effort

- Phase 1: 4-6 hours (6 engines × 3-4 tests each)
- Phase 2: 3-4 hours (6 engines × 2-3 tests each)
- Phase 3: 2-3 hours (comparison tests)
- Phase 4: 3-4 hours (complex scenarios)
- Phase 5: 2-3 hours (extend existing tests)
- Phase 6: 3-4 hours (cross-table scenarios)

**Total: 17-24 hours**

## Notes

- Many low-level components are well-tested (VersionChain, ConflictDetector, Snapshot)
- Main gaps are in end-to-end integration tests
- Need to test MVCC behavior at Database API level
- Need to verify behavior across all storage engines
- Focus on user-visible guarantees, not just internal correctness

---
Made with Bob