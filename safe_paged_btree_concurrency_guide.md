# Adapting BzTree-Style Concurrent Mutation to a Safe Rust Paged On-Disk B-Tree

## Purpose

This guide explains how to adapt the useful ideas from BzTree-style concurrent mutation into a **paged, on-disk B-tree implemented in Rust without `unsafe` code**.

The goal is not to copy BzTree literally. BzTree is an in-memory structure built around pointer mutation, multi-word compare-and-swap, node freezing, append-style updates, and epoch reclamation. A paged disk B-tree has different constraints:

- Pointers become stable `PageId`s.
- In-memory replacement becomes page-version replacement.
- MwCAS becomes a combination of latching, validation, WAL, and atomic page publication.
- Epoch reclamation becomes page version retention and safe reclamation after checkpoint.
- Returned references cannot point directly into mutable page buffers unless their lifetimes are tightly guarded.

The safest adaptation is to preserve the **algorithmic shape**:

```text
read optimistically
validate before publishing
modify by copy/build/replacement where possible
publish atomically through WAL + page table update
retire old versions only after no reader can observe them
```

But implement it with safe Rust abstractions.

---

## 1. Core Design Shift: From Pointer CAS to Page-Version Publishing

BzTree mutates memory-resident nodes by installing new pointers and metadata using multi-word CAS. A disk B-tree should instead use **page versions**.

Instead of this:

```text
CAS(parent.child_ptr, old_node_ptr, new_node_ptr)
```

Use this:

```text
allocate new child page version
write redo/undo information to WAL
validate parent version
install child PageId/version in parent
write parent page version
commit transaction
```

There are two broad implementation models.

### Model A: In-place pages with WAL

Pages have stable `PageId`s. Mutations update page bytes in place after logging redo/undo records.

```text
PageId 42 remains PageId 42
contents are updated in place
WAL guarantees recovery
latches prevent conflicting writers
```

This resembles traditional database B-trees.

Pros:

- Space efficient.
- Familiar.
- Good for small point updates.

Cons:

- More difficult recovery logic.
- More complicated split/merge correctness.
- Readers need page latches or version validation.

### Model B: Copy-on-write page versions

Each mutation writes a new physical page version, then publishes it through a page table or parent pointer update.

```text
logical PageId 42 -> physical frame/version A
mutation creates physical frame/version B
commit publishes logical PageId 42 -> version B
```

Pros:

- Cleaner reader isolation.
- Natural fit for optimistic concurrency.
- Easier crash consistency if publication is atomic.

Cons:

- More write amplification.
- Requires garbage collection of old page versions.
- Requires page map / manifest machinery.

### Recommended hybrid

For an embedded single-file database, a practical middle ground is:

```text
logical PageId remains stable
page contents are rewritten as new physical versions
WAL records intent and commit
page table maps logical PageId -> physical location/version
old versions retained until checkpoint/reclamation
```

This keeps B-tree references stable while allowing safe copy-build-publish mutation.

---

## 2. Safe Rust Building Blocks

Avoid returning raw references into pages unless they are bound to a read guard. Prefer owned values or short-lived guard-bound views.

### Page identifiers

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PageId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PageVersion(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageRef {
    pub id: PageId,
    pub version: PageVersion,
}
```

### Page kind

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageKind {
    Leaf,
    Internal,
}
```

### Page header

```rust
#[derive(Debug, Clone)]
pub struct PageHeader {
    pub page_id: PageId,
    pub version: PageVersion,
    pub kind: PageKind,
    pub key_count: u16,
    pub right_sibling: Option<PageRef>,
    pub low_key: Option<Vec<u8>>,
    pub high_key: Option<Vec<u8>>,
}
```

`low_key` and `high_key` are optional fence keys. They are extremely useful for safe concurrent traversal because they let a reader detect that it has landed on a page that no longer owns the searched key range.

### Leaf and internal pages

```rust
#[derive(Debug, Clone)]
pub struct LeafEntry {
    pub key: Vec<u8>,
    pub value: Vec<u8>,
    pub tombstone: bool,
}

#[derive(Debug, Clone)]
pub struct InternalEntry {
    pub separator: Vec<u8>,
    pub child: PageRef,
}

#[derive(Debug, Clone)]
pub enum BTreePage {
    Leaf {
        header: PageHeader,
        entries: Vec<LeafEntry>,
    },
    Internal {
        header: PageHeader,
        children: Vec<InternalEntry>,
    },
}
```

This representation is not the most compact disk format, but it is a clean logical representation. Serialization can transform this into packed page bytes.

---

## 3. Important Concurrency Concepts

### Latch versus lock

In database language, a **latch** protects in-memory page state briefly. A **lock** protects logical records or transactions.

For the storage engine, you usually need latches first.

```text
latch = short critical section for page mutation
lock  = logical isolation across user transactions
```

This guide focuses on latches and structural correctness, not full SQL-style transactional isolation.

### Optimistic read path

Reads should avoid exclusive latches.

A safe optimistic read does this:

```text
1. Load root PageRef.
2. Read page snapshot.
3. Check page version and fence keys.
4. Descend to child.
5. Repeat until leaf.
6. Validate leaf still owns key range.
7. Return owned value or guarded view.
```

The reader does not need to block writers if page versions remain readable until no reader can observe them.

### Writer path

Writers should use short-lived exclusive page latches.

```text
1. Traverse optimistically and record path.
2. Latch target leaf.
3. Validate leaf version and fence ownership.
4. Modify leaf or split.
5. Latch parent only when needed.
6. Validate parent version.
7. Publish changes through WAL and page table.
8. Release latches.
```

This is similar to BzTree’s “find, validate, freeze/replace,” but adapted to pages.

---

## 4. Page Cache Architecture Without Unsafe Code

Use `Arc`, `RwLock`, `Mutex`, channels, and owned page buffers. You can build a correct version first and optimize later.

### Page cache traits

```rust
use std::sync::Arc;

pub trait PageCache {
    fn get(&self, page: PageRef) -> Result<Arc<CachedPage>, StorageError>;
    fn current(&self, id: PageId) -> Result<PageRef, StorageError>;
    fn publish(&self, id: PageId, expected: PageVersion, new_ref: PageRef) -> Result<(), PublishError>;
}

pub struct CachedPage {
    pub page_ref: PageRef,
    pub page: BTreePage,
}
```

The simple safe design is immutable cached pages:

```text
CachedPage is immutable once created.
A mutation creates a new CachedPage.
Publishing swaps the current PageRef for the logical PageId.
```

### Page table

```rust
use std::collections::HashMap;
use std::sync::RwLock;

pub struct PageTable {
    current: RwLock<HashMap<PageId, PageRef>>,
}
```

Publishing is a compare-and-swap at the logical page level:

```rust
impl PageTable {
    pub fn publish(
        &self,
        id: PageId,
        expected: PageVersion,
        replacement: PageRef,
    ) -> Result<(), PublishError> {
        let mut table = self.current.write().unwrap();
        let current = table.get(&id).copied().ok_or(PublishError::MissingPage)?;

        if current.version != expected {
            return Err(PublishError::Conflict { current });
        }

        table.insert(id, replacement);
        Ok(())
    }
}
```

This is safe Rust and gives you optimistic conflict detection. Later, this can be replaced with sharded locks or lock-free maps if needed.

---

## 5. WAL Design

For on-disk mutation, correctness depends on the WAL.

A minimal WAL should support:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Lsn(pub u64);

#[derive(Debug, Clone)]
pub enum WalRecord {
    Begin {
        tx_id: u64,
    },
    PutPageVersion {
        tx_id: u64,
        logical_page_id: PageId,
        old_version: PageVersion,
        new_version: PageVersion,
        page_bytes: Vec<u8>,
    },
    PublishPage {
        tx_id: u64,
        logical_page_id: PageId,
        expected_version: PageVersion,
        new_version: PageVersion,
    },
    RootUpdate {
        tx_id: u64,
        old_root: PageRef,
        new_root: PageRef,
    },
    Commit {
        tx_id: u64,
    },
    Abort {
        tx_id: u64,
    },
}
```

### Write ordering

The safe write order is:

```text
1. Append Begin.
2. Append PutPageVersion records for every new page image.
3. Flush WAL.
4. Write new page images to data file or append-only page area.
5. Append PublishPage / RootUpdate records.
6. Append Commit.
7. Flush WAL.
8. Apply in-memory page table updates.
```

A more optimized engine can relax some of this, but this order is easier to reason about.

### Recovery

On startup:

```text
1. Read last checkpoint.
2. Replay committed WAL records after checkpoint.
3. Ignore uncommitted transactions.
4. Reconstruct page table and root PageRef.
5. Reclaim unreferenced page versions later.
```

---

## 6. Adapting BzTree Insert to Paged B-Tree Insert

BzTree insert reserves a slot, writes the key/value, then publishes visibility. For disk pages, do not reserve slots inside a mutable node unless you are building an in-memory memtable. Instead, use page copy mutation.

### Insert algorithm

```text
insert(key, value):
  loop:
    path = optimistic_find_leaf(key)
    leaf_ref = path.leaf
    leaf = read_current(leaf_ref)

    if !leaf.owns(key):
        continue

    if leaf.has_space_for(key, value):
        new_leaf = leaf.clone_with_insert_or_replace(key, value)
        wal_publish_single_page(leaf_ref, new_leaf)
        return Ok

    else:
        split = leaf.clone_with_split_insert(key, value)
        result = install_split(path, split)
        if result == Conflict:
            continue
        return Ok
```

### Key point

The copied page is prepared privately, then published atomically. Other threads either see:

```text
old leaf
```

or:

```text
new leaf
```

They never see a half-written page.

---

## 7. Split Protocol

A split replaces one leaf with two leaves and updates the parent.

### Leaf split output

```rust
pub struct LeafSplit {
    pub old_leaf: PageRef,
    pub left: BTreePage,
    pub right: BTreePage,
    pub separator: Vec<u8>,
}
```

### Split steps

```text
1. Read and validate leaf.
2. Build left and right pages from visible entries plus new key/value.
3. Allocate new PageIds or new PageVersions.
4. Latch/validate parent.
5. Insert separator and right child into parent.
6. If parent has room, publish leaf replacements and parent replacement in one mini-transaction.
7. If parent overflows, recursively split parent.
8. If root overflows, create a new root.
```

### Without MwCAS

You cannot atomically modify several pages with CPU CAS. The WAL transaction becomes your atomic unit.

For example, split publication is one storage transaction:

```text
Begin tx
PutPageVersion old_leaf -> left_version
PutPageVersion new_right_page -> right_version
PutPageVersion parent -> parent_version_with_separator
Commit tx
Publish in-memory page table updates
```

On recovery, either all committed page versions are installed or none are.

---

## 8. Parent Update Strategy

There are two common approaches.

### Approach A: Crabbing / latch coupling

Hold the parent latch while descending if the child may split.

```text
latch parent
latch child
if child is safe:
  release parent
modify child
else:
  split while parent remains latched
```

Pros:

- Simpler structural correctness.
- Traditional B-tree approach.

Cons:

- More blocking.
- More latch contention near root.

### Approach B: Optimistic path validation

Traverse without exclusive parent latches, record the path, then latch and validate only when needed.

```text
path = [(page_id, version, child_index), ...]
latch leaf
if split needed:
  latch parent from path
  validate parent version and child pointer
  install split
```

Pros:

- Better concurrency.
- Closer to BzTree.

Cons:

- More retries.
- Harder implementation.

### Recommended path

Start with **optimistic path validation**, but fall back to restart on any conflict.

Conflict is not failure. Conflict means:

```text
someone else modified the structure; restart from root
```

---

## 9. Fence Keys and Sibling Links

Fence keys are one of the most important details for concurrent B-trees.

Each page should know the key range it owns:

```text
low_key <= key < high_key
```

If a reader reaches a page and the key is outside the fence range, it can recover.

### Right sibling links

Leaf pages should have right sibling links:

```rust
right_sibling: Option<PageRef>
```

During split:

```text
old page range: [A, Z)
left range:     [A, M)
right range:    [M, Z)
left.right_sibling = right
right.right_sibling = old.right_sibling
```

If a reader lands on the old left side while the parent has not yet been observed updated, it can follow right links until it finds the correct fence range.

This is the core idea behind B-link trees and is very useful in concurrent disk B-trees.

---

## 10. Delete and Tombstones

For a disk B-tree, deletion can be implemented in stages.

### Simple stage-one delete

Do not immediately merge pages.

```text
1. Find leaf.
2. Copy leaf.
3. Mark key as tombstone or remove key.
4. Publish new leaf version.
```

If you support MVCC or snapshots, tombstones are safer than physical removal.

### Compaction

When a page accumulates too many tombstones:

```text
copy live entries into a compacted page
publish compacted page version
retire old version later
```

### Merge

Merging underfull pages is optional at first.

Many production engines delay merge because aggressive merge causes write amplification and contention. A safe initial policy is:

```text
split eagerly
compact occasionally
merge lazily in background
```

---

## 11. Merge Protocol

When implementing merge, use the same copy-build-publish model.

```text
1. Find underfull leaf.
2. Latch leaf and sibling in stable order by PageId.
3. Validate both versions and fence adjacency.
4. If combined entries fit, build merged page.
5. Latch parent.
6. Validate parent references both pages.
7. Publish merged page and parent replacement in one WAL transaction.
8. Retire old page versions.
```

Stable latch ordering is essential:

```text
always latch lower PageId first
```

This prevents deadlocks.

---

## 12. Root Updates

Root updates need special care because every traversal starts there.

Use a root pointer protected by an atomic-ish safe abstraction:

```rust
use std::sync::RwLock;

pub struct RootRef {
    inner: RwLock<PageRef>,
}
```

Root split:

```text
1. Split old root into left and right.
2. Create new internal root.
3. WAL records all new page versions.
4. WAL records RootUpdate old_root -> new_root.
5. Commit.
6. Update RootRef.
```

Root contraction after merge is optional. You can postpone it.

---

## 13. Safe Return Values

Do not return `&[u8]` into a page cache unless you have a guard type that holds the page alive.

Simplest safe API:

```rust
pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError>
```

More advanced API:

```rust
pub struct ValueGuard {
    page: Arc<CachedPage>,
    value_range: std::ops::Range<usize>,
}

impl ValueGuard {
    pub fn as_bytes(&self) -> &[u8] {
        // safe because self owns Arc<CachedPage>
        todo!()
    }
}
```

Returning owned `Vec<u8>` first is usually the right tradeoff.

---

## 14. Suggested Module Layout

```text
src/
  lib.rs
  page/
    mod.rs
    id.rs
    header.rs
    codec.rs
    layout.rs
  btree/
    mod.rs
    cursor.rs
    search.rs
    insert.rs
    delete.rs
    split.rs
    merge.rs
    validate.rs
  cache/
    mod.rs
    page_cache.rs
    page_table.rs
    eviction.rs
  wal/
    mod.rs
    record.rs
    writer.rs
    replay.rs
  txn/
    mod.rs
    mini_tx.rs
  io/
    mod.rs
    file.rs
    checksum.rs
  error.rs
```

---

## 15. Minimal Traits

### Codec

```rust
pub trait PageCodec {
    fn encode(&self, page: &BTreePage, out: &mut Vec<u8>) -> Result<(), StorageError>;
    fn decode(&self, page_id: PageId, version: PageVersion, bytes: &[u8]) -> Result<BTreePage, StorageError>;
}
```

### Page allocator

```rust
pub trait PageAllocator {
    fn allocate(&self) -> Result<PageId, StorageError>;
    fn next_version(&self, page_id: PageId) -> Result<PageVersion, StorageError>;
}
```

### WAL

```rust
pub trait Wal {
    fn begin(&self) -> Result<u64, StorageError>;
    fn append(&self, tx_id: u64, record: WalRecord) -> Result<Lsn, StorageError>;
    fn flush(&self, lsn: Lsn) -> Result<(), StorageError>;
    fn commit(&self, tx_id: u64) -> Result<Lsn, StorageError>;
}
```

### B-tree API

```rust
pub trait OrderedIndex {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError>;
    fn insert(&self, key: Vec<u8>, value: Vec<u8>) -> Result<(), StorageError>;
    fn delete(&self, key: &[u8]) -> Result<bool, StorageError>;
}
```

---

## 16. Insert Pseudocode

```rust
pub fn insert(&self, key: Vec<u8>, value: Vec<u8>) -> Result<(), StorageError> {
    loop {
        let path = self.find_leaf_path(&key)?;
        let leaf_ref = path.leaf_ref();
        let leaf = self.cache.get(leaf_ref)?;

        if !leaf.page.owns_key(&key) {
            continue;
        }

        if leaf.page.has_space_for(&key, &value) {
            let new_leaf = leaf.page.clone_with_insert(key.clone(), value.clone())?;

            match self.publish_page_replacement(leaf_ref, new_leaf)? {
                PublishOutcome::Published => return Ok(()),
                PublishOutcome::Conflict => continue,
            }
        }

        let split = leaf.page.clone_with_split_insert(key.clone(), value.clone())?;

        match self.install_split(path, split)? {
            PublishOutcome::Published => return Ok(()),
            PublishOutcome::Conflict => continue,
        }
    }
}
```

No unsafe code is needed. The cost is cloning page contents during mutation.

---

## 17. Single-Page Publish Pseudocode

```rust
fn publish_page_replacement(
    &self,
    old_ref: PageRef,
    new_page: BTreePage,
) -> Result<PublishOutcome, StorageError> {
    let tx_id = self.wal.begin()?;
    let new_version = self.allocator.next_version(old_ref.id)?;
    let new_ref = PageRef { id: old_ref.id, version: new_version };

    let bytes = self.codec.encode_to_vec(&new_page)?;

    let lsn = self.wal.append(
        tx_id,
        WalRecord::PutPageVersion {
            tx_id,
            logical_page_id: old_ref.id,
            old_version: old_ref.version,
            new_version,
            page_bytes: bytes,
        },
    )?;

    self.wal.flush(lsn)?;
    self.page_file.write_page_version(new_ref, &new_page)?;

    let lsn = self.wal.append(
        tx_id,
        WalRecord::PublishPage {
            tx_id,
            logical_page_id: old_ref.id,
            expected_version: old_ref.version,
            new_version,
        },
    )?;

    let commit_lsn = self.wal.commit(tx_id)?;
    self.wal.flush(commit_lsn)?;

    match self.page_table.publish(old_ref.id, old_ref.version, new_ref) {
        Ok(()) => Ok(PublishOutcome::Published),
        Err(PublishError::Conflict { .. }) => {
            // Another writer won the race. The committed but unpublished page version
            // should be treated as unreachable and reclaimed later, or the publish
            // check should happen before commit under a page latch.
            Ok(PublishOutcome::Conflict)
        }
        Err(e) => Err(e.into()),
    }
}
```

In a real implementation, avoid committing a page version that cannot be published. The better design is to validate and publish under the page table write latch before final commit, or include enough recovery metadata to determine whether the publish happened.

---

## 18. Better Mini-Transaction Publish

A cleaner design is:

```text
1. Acquire logical page publish latch.
2. Validate current version.
3. Write WAL and page image.
4. Commit WAL.
5. Update page table.
6. Release latch.
```

This prevents orphan committed versions caused by publish conflict.

```rust
fn publish_under_latch(
    &self,
    old_ref: PageRef,
    new_page: BTreePage,
) -> Result<PublishOutcome, StorageError> {
    let _guard = self.latches.exclusive(old_ref.id);

    let current = self.page_table.current(old_ref.id)?;
    if current.version != old_ref.version {
        return Ok(PublishOutcome::Conflict);
    }

    let tx_id = self.wal.begin()?;
    let new_ref = self.write_new_page_version(tx_id, old_ref, new_page)?;
    self.wal.append(tx_id, WalRecord::PublishPage {
        tx_id,
        logical_page_id: old_ref.id,
        expected_version: old_ref.version,
        new_version: new_ref.version,
    })?;
    let commit_lsn = self.wal.commit(tx_id)?;
    self.wal.flush(commit_lsn)?;

    self.page_table.publish(old_ref.id, old_ref.version, new_ref)?;
    Ok(PublishOutcome::Published)
}
```

This is the recommended starting point.

---

## 19. Latch Manager Without Unsafe

A simple latch manager can use striped `RwLock`s.

```rust
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

pub struct LatchManager {
    stripes: Vec<Arc<RwLock<()>>>,
}

impl LatchManager {
    pub fn new(stripes: usize) -> Self {
        let stripes = (0..stripes)
            .map(|_| Arc::new(RwLock::new(())))
            .collect();
        Self { stripes }
    }

    fn stripe(&self, page_id: PageId) -> &Arc<RwLock<()>> {
        let index = page_id.0 as usize % self.stripes.len();
        &self.stripes[index]
    }

    pub fn read(&self, page_id: PageId) -> RwLockReadGuard<'_, ()> {
        self.stripe(page_id).read().unwrap()
    }

    pub fn write(&self, page_id: PageId) -> RwLockWriteGuard<'_, ()> {
        self.stripe(page_id).write().unwrap()
    }
}
```

This is not perfect because unrelated pages can collide on the same stripe, but it is simple, safe, and effective enough for a first implementation.

For higher concurrency, use a lock table:

```text
DashMap<PageId, Arc<RwLock<()>>>
```

or a custom sharded map using only safe Rust.

---

## 20. Deadlock Avoidance

Rules:

1. Do not hold latches while doing slow disk I/O if avoidable.
2. When latching siblings, acquire in ascending `PageId` order.
3. When latching parent and child, prefer top-down order.
4. If order would be violated, release and restart.
5. Treat restart as normal control flow.

For optimistic split:

```text
latch child
need parent
try_latch parent
if unavailable:
  release child
  restart
```

This avoids deadlock at the cost of retries.

---

## 21. Comparison to BzTree Concepts

| BzTree concept | Safe paged B-tree adaptation |
|---|---|
| Heap pointer | `PageRef { PageId, PageVersion }` |
| MwCAS | WAL mini-transaction + validation latch |
| Node freeze | Page publish latch / structural modification latch |
| Append unsorted delta | Copy page and insert into sorted page, or use slotted-page delta region |
| Entry metadata | Tombstone/version metadata in page entry |
| Epoch reclamation | Retain old page versions until checkpoint / no active readers |
| Root pointer CAS | WAL `RootUpdate` + guarded root ref update |
| In-memory split | Build new page versions, publish parent update |
| In-memory merge | Build merged page version, publish parent update |

---

## 22. Optional Delta-Page Variant

If copying a whole page for every update is too expensive, you can adapt BzTree’s append-delta idea to disk pages.

Page layout:

```text
header
sorted entry table
delta entry table
free space
value area
```

Insert:

```text
append delta record
mark visible after WAL commit
```

Search:

```text
scan delta area newest to oldest
then binary-search sorted area
```

Compaction:

```text
merge sorted entries + visible deltas into a new sorted page
remove tombstones
publish compacted page version
```

This is closer to BzTree, but harder to implement. Start with copy-on-write pages first.

---

## 23. Testing Strategy

### Unit tests

- Page encoding/decoding round trip.
- Insert into empty leaf.
- Insert until split.
- Root split.
- Internal split.
- Delete existing key.
- Delete missing key.
- Fence key validation.
- Sibling traversal after split.

### Concurrency tests

Use `loom` for small state-space tests if possible.

Scenarios:

```text
reader during leaf replacement
writer/writer same leaf conflict
writer/writer different leaves
split while reader descends stale parent
split while another writer inserts into old leaf
merge while reader follows sibling
root split while reader starts traversal
```

### Crash tests

Build a deterministic fault injector:

```text
crash after WAL begin
crash after page image write
crash after publish record
crash after commit record
crash after page table update
```

After recovery, verify:

```text
all committed keys are visible
no uncommitted keys are visible
B-tree invariants hold
all page checksums pass
root is reachable
no reachable page has invalid fence keys
```

---

## 24. B-Tree Invariants to Check

A debug verifier should check:

```text
1. Root exists.
2. Every child page is reachable from exactly one parent, unless sibling links are counted separately.
3. Keys inside each page are sorted.
4. Every child range is within parent separator bounds.
5. Fence keys are correct.
6. Leaf sibling chain is ordered.
7. No page exceeds max size.
8. Non-root pages are not below minimum occupancy, unless lazy merge is enabled.
9. Parent child PageRefs match current page table versions, or intentionally reference stable historical versions.
10. All page checksums are valid.
```

---

## 25. Practical Recommendation

For a first safe Rust implementation, use this progression:

### Phase 1: Single-threaded copy-on-write B-tree

- Page IDs.
- Page codec.
- Root update.
- Insert/get/delete.
- Split.
- No merge.

### Phase 2: WAL and recovery

- WAL records.
- Commit protocol.
- Checkpoint.
- Crash tests.

### Phase 3: Safe concurrent reads

- Immutable cached page versions.
- Root `RwLock<PageRef>`.
- Page table `RwLock<HashMap<PageId, PageRef>>`.
- Owned return values.

### Phase 4: Concurrent writers

- Page latch manager.
- Optimistic path validation.
- Conflict retry.
- Split install with parent validation.

### Phase 5: Compaction and lazy merge

- Tombstones.
- Page compaction.
- Background merge.
- Old version reclamation.

### Phase 6: Performance work

- Slotted-page encoding.
- Prefix compression.
- Delta records.
- Group commit.
- Sharded page table.
- Buffer cache eviction.
- Range scan cursor guards.

---

## 26. Design Summary

The safest way to adapt BzTree-like concurrent mutation to a paged on-disk Rust B-tree is:

```text
Do not mutate shared page bytes in place.
Build replacement pages privately.
Validate the observed page versions before publishing.
Use WAL as the atomic multi-page commit mechanism.
Publish new PageRefs through a guarded page table/root pointer.
Keep old page versions alive until readers and recovery no longer need them.
Use retries instead of complex blocking wherever possible.
```

This gives you many of BzTree’s best properties — optimistic traversal, copy-build-publish mutation, structural replacement, and safe reclamation — while fitting the realities of disk pages, crash recovery, and safe Rust.

