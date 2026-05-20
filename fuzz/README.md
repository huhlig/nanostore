# Fuzzing Harnesses for nanostore

This directory contains cargo-fuzz harnesses for testing the robustness of nanostore's parsing and serialization code.

## Prerequisites

Install cargo-fuzz if you haven't already:

```bash
cargo install cargo-fuzz
```

## Available Fuzz Targets

### 1. `fuzz_page_parsing`

Tests the page parsing and serialization logic in the pager module.

**What it tests:**
- Page header parsing with malformed data
- Full page parsing with and without checksum verification
- Compression edge cases (LZ4, Zstd)
- Encryption edge cases (AES-256-GCM)
- Truncated inputs at various boundaries
- Overflow page header parsing
- Page type conversion with arbitrary bytes

**Run it:**
```bash
cargo fuzz run fuzz_page_parsing
```

**Run with specific options:**
```bash
# Run for 60 seconds
cargo fuzz run fuzz_page_parsing -- -max_total_time=60

# Run with multiple jobs (parallel fuzzing)
cargo fuzz run fuzz_page_parsing -- -jobs=4

# Run with a specific corpus directory
cargo fuzz run fuzz_page_parsing -- corpus/fuzz_page_parsing
```

### 2. `fuzz_wal_parsing`

Tests the WAL (Write-Ahead Log) record parsing and serialization logic.

**What it tests:**
- WAL record parsing with arbitrary bytes
- Record type and write operation type conversion
- Compression combinations (None, LZ4, Zstd)
- Encryption combinations (None, AES-256-GCM)
- Checksum validation with corrupted data
- All record types (Begin, Write, Commit, Rollback, Checkpoint, Prepare)
- All write operation types (Put, Delete, Bloom, Graph, TimeSeries, Vector, Geo, FullText)
- Truncated inputs at various boundaries
- Wrong encryption keys

**Run it:**
```bash
cargo fuzz run fuzz_wal_parsing
```

**Run with specific options:**
```bash
# Run for 60 seconds
cargo fuzz run fuzz_wal_parsing -- -max_total_time=60

# Run with multiple jobs (parallel fuzzing)
cargo fuzz run fuzz_wal_parsing -- -jobs=4

# Run with a specific corpus directory
cargo fuzz run fuzz_wal_parsing -- corpus/fuzz_wal_parsing
```

## Common Fuzzing Options

### Time Limits
```bash
# Run for 5 minutes
cargo fuzz run <target> -- -max_total_time=300

# Run for 1 hour
cargo fuzz run <target> -- -max_total_time=3600
```

### Parallel Fuzzing
```bash
# Use 4 parallel jobs
cargo fuzz run <target> -- -jobs=4

# Use all available CPU cores
cargo fuzz run <target> -- -jobs=$(nproc)
```

### Memory Limits
```bash
# Limit memory to 2GB
cargo fuzz run <target> -- -rss_limit_mb=2048
```

### Corpus Management
```bash
# Minimize the corpus (remove redundant inputs)
cargo fuzz cmin <target>

# Merge multiple corpus directories
cargo fuzz cmin <target> corpus1 corpus2 corpus3
```

## Interpreting Results

### Crashes
If a fuzzer finds a crash, it will:
1. Save the crashing input to `fuzz/artifacts/<target>/crash-<hash>`
2. Print a stack trace
3. Exit with a non-zero status

To reproduce a crash:
```bash
cargo fuzz run <target> fuzz/artifacts/<target>/crash-<hash>
```

### Coverage
To see code coverage:
```bash
cargo fuzz coverage <target>
```

### Continuous Fuzzing
For long-running fuzzing campaigns, consider:
1. Running multiple jobs in parallel
2. Periodically minimizing the corpus
3. Monitoring for crashes and hangs
4. Backing up the corpus directory

## Integration with CI/CD

Example GitHub Actions workflow snippet:

```yaml
- name: Run fuzz tests
  run: |
    cargo install cargo-fuzz
    cargo fuzz run fuzz_page_parsing -- -max_total_time=60
    cargo fuzz run fuzz_wal_parsing -- -max_total_time=60
```

## Troubleshooting

### "error: no such subcommand: `fuzz`"
Install cargo-fuzz: `cargo install cargo-fuzz`

### Out of memory errors
Reduce the RSS limit: `cargo fuzz run <target> -- -rss_limit_mb=1024`

### Slow fuzzing
- Use parallel jobs: `-jobs=4`
- Minimize the corpus: `cargo fuzz cmin <target>`
- Use a faster build: `cargo fuzz run <target> --release`

## Further Reading

- [cargo-fuzz documentation](https://rust-fuzz.github.io/book/cargo-fuzz.html)
- [libFuzzer options](https://llvm.org/docs/LibFuzzer.html#options)
- [Fuzzing best practices](https://rust-fuzz.github.io/book/introduction.html)