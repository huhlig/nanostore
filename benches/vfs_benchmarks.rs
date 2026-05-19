//
// Copyright 2025-2026 Hans W. Uhlig. All Rights Reserved.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//

//! Benchmarks for VFS implementations

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use nanostore::vfs::{File, FileSystem, LocalFileSystem, MemoryFileSystem};
use std::hint::black_box;
use std::io::{Read, Seek, SeekFrom, Write};

// ============================================================================
// File Creation Benchmarks
// ============================================================================

fn bench_file_creation(c: &mut Criterion) {
    let mut group = c.benchmark_group("file_creation");

    group.bench_function("memory_fs", |b| {
        let fs = MemoryFileSystem::new();
        let mut counter = 0;
        b.iter(|| {
            let path = format!("/bench_{}.txt", counter);
            counter += 1;
            let file = fs.create_file(&path).unwrap();
            black_box(file);
        });
    });

    group.bench_function("local_fs", |b| {
        let temp_dir = tempfile::tempdir().unwrap();
        let fs = LocalFileSystem::new(temp_dir.path());
        let mut counter = 0;
        b.iter(|| {
            let path = format!("/bench_{}.txt", counter);
            counter += 1;
            let file = fs.create_file(&path).unwrap();
            black_box(file);
        });
    });

    group.finish();
}

// ============================================================================
// Sequential Write Benchmarks
// ============================================================================

fn bench_sequential_write(c: &mut Criterion) {
    let mut group = c.benchmark_group("sequential_write");

    for size in [1024, 4096, 16384, 65536].iter() {
        group.throughput(Throughput::Bytes(*size as u64));

        group.bench_with_input(BenchmarkId::new("memory_fs", size), size, |b, &size| {
            let fs = MemoryFileSystem::new();
            let data = vec![0xAB; size];
            b.iter(|| {
                let mut file = fs.create_file("/bench.txt").unwrap();
                file.write_all(&data).unwrap();
                black_box(file);
                fs.remove_file("/bench.txt").unwrap();
            });
        });

        group.bench_with_input(BenchmarkId::new("local_fs", size), size, |b, &size| {
            let temp_dir = tempfile::tempdir().unwrap();
            let fs = LocalFileSystem::new(temp_dir.path());
            let data = vec![0xAB; size];
            b.iter(|| {
                let mut file = fs.create_file("/bench.txt").unwrap();
                file.write_all(&data).unwrap();
                black_box(file);
                fs.remove_file("/bench.txt").unwrap();
            });
        });
    }

    group.finish();
}

// ============================================================================
// Sequential Read Benchmarks
// ============================================================================

fn bench_sequential_read(c: &mut Criterion) {
    let mut group = c.benchmark_group("sequential_read");

    for size in [1024, 4096, 16384, 65536].iter() {
        group.throughput(Throughput::Bytes(*size as u64));

        group.bench_with_input(BenchmarkId::new("memory_fs", size), size, |b, &size| {
            let fs = MemoryFileSystem::new();
            let data = vec![0xAB; size];
            let mut file = fs.create_file("/bench.txt").unwrap();
            file.write_all(&data).unwrap();
            drop(file);

            b.iter(|| {
                let mut file = fs.open_file("/bench.txt").unwrap();
                let mut buffer = vec![0u8; size];
                file.read_exact(&mut buffer).unwrap();
                black_box(buffer);
            });
        });

        group.bench_with_input(BenchmarkId::new("local_fs", size), size, |b, &size| {
            let temp_dir = tempfile::tempdir().unwrap();
            let fs = LocalFileSystem::new(temp_dir.path());
            let data = vec![0xAB; size];
            let mut file = fs.create_file("/bench.txt").unwrap();
            file.write_all(&data).unwrap();
            drop(file);

            b.iter(|| {
                let mut file = fs.open_file("/bench.txt").unwrap();
                let mut buffer = vec![0u8; size];
                file.read_exact(&mut buffer).unwrap();
                black_box(buffer);
            });
        });
    }

    group.finish();
}

// ============================================================================
// Random Access Benchmarks
// ============================================================================

fn bench_random_access(c: &mut Criterion) {
    let mut group = c.benchmark_group("random_access");

    let file_size = 65536;
    let read_size = 1024;

    group.bench_function("memory_fs_read_at_offset", |b| {
        let fs = MemoryFileSystem::new();
        let data = vec![0xAB; file_size];
        let mut file = fs.create_file("/bench.txt").unwrap();
        file.write_all(&data).unwrap();

        b.iter(|| {
            let mut buffer = vec![0u8; read_size];
            file.read_at_offset(black_box(1024), &mut buffer).unwrap();
            black_box(buffer);
        });
    });

    group.bench_function("local_fs_read_at_offset", |b| {
        let temp_dir = tempfile::tempdir().unwrap();
        let fs = LocalFileSystem::new(temp_dir.path());
        let data = vec![0xAB; file_size];
        let mut file = fs.create_file("/bench.txt").unwrap();
        file.write_all(&data).unwrap();

        b.iter(|| {
            let mut buffer = vec![0u8; read_size];
            file.read_at_offset(black_box(1024), &mut buffer).unwrap();
            black_box(buffer);
        });
    });

    group.bench_function("memory_fs_write_to_offset", |b| {
        let fs = MemoryFileSystem::new();
        let data = vec![0xAB; file_size];
        let mut file = fs.create_file("/bench.txt").unwrap();
        file.write_all(&data).unwrap();
        let write_data = vec![0xCD; read_size];

        b.iter(|| {
            file.write_to_offset(black_box(1024), &write_data).unwrap();
        });
    });

    group.bench_function("local_fs_write_to_offset", |b| {
        let temp_dir = tempfile::tempdir().unwrap();
        let fs = LocalFileSystem::new(temp_dir.path());
        let data = vec![0xAB; file_size];
        let mut file = fs.create_file("/bench.txt").unwrap();
        file.write_all(&data).unwrap();
        let write_data = vec![0xCD; read_size];

        b.iter(|| {
            file.write_to_offset(black_box(1024), &write_data).unwrap();
        });
    });

    group.finish();
}

// ============================================================================
// Seek Benchmarks
// ============================================================================

fn bench_seek_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("seek_operations");

    let file_size = 65536;

    group.bench_function("memory_fs_seek_start", |b| {
        let fs = MemoryFileSystem::new();
        let data = vec![0xAB; file_size];
        let mut file = fs.create_file("/bench.txt").unwrap();
        file.write_all(&data).unwrap();

        b.iter(|| {
            file.seek(SeekFrom::Start(black_box(1024))).unwrap();
        });
    });

    group.bench_function("local_fs_seek_start", |b| {
        let temp_dir = tempfile::tempdir().unwrap();
        let fs = LocalFileSystem::new(temp_dir.path());
        let data = vec![0xAB; file_size];
        let mut file = fs.create_file("/bench.txt").unwrap();
        file.write_all(&data).unwrap();

        b.iter(|| {
            file.seek(SeekFrom::Start(black_box(1024))).unwrap();
        });
    });

    group.bench_function("memory_fs_seek_end", |b| {
        let fs = MemoryFileSystem::new();
        let data = vec![0xAB; file_size];
        let mut file = fs.create_file("/bench.txt").unwrap();
        file.write_all(&data).unwrap();

        b.iter(|| {
            file.seek(SeekFrom::End(black_box(-1024))).unwrap();
        });
    });

    group.bench_function("local_fs_seek_end", |b| {
        let temp_dir = tempfile::tempdir().unwrap();
        let fs = LocalFileSystem::new(temp_dir.path());
        let data = vec![0xAB; file_size];
        let mut file = fs.create_file("/bench.txt").unwrap();
        file.write_all(&data).unwrap();

        b.iter(|| {
            file.seek(SeekFrom::End(black_box(-1024))).unwrap();
        });
    });

    group.finish();
}

// ============================================================================
// File Resize Benchmarks
// ============================================================================

fn bench_file_resize(c: &mut Criterion) {
    let mut group = c.benchmark_group("file_resize");

    group.bench_function("memory_fs_grow", |b| {
        let fs = MemoryFileSystem::new();
        b.iter(|| {
            let mut file = fs.create_file("/bench.txt").unwrap();
            file.set_size(black_box(65536)).unwrap();
            black_box(file);
            fs.remove_file("/bench.txt").unwrap();
        });
    });

    group.bench_function("local_fs_grow", |b| {
        let temp_dir = tempfile::tempdir().unwrap();
        let fs = LocalFileSystem::new(temp_dir.path());
        b.iter(|| {
            let mut file = fs.create_file("/bench.txt").unwrap();
            file.set_size(black_box(65536)).unwrap();
            black_box(file);
            fs.remove_file("/bench.txt").unwrap();
        });
    });

    group.bench_function("memory_fs_shrink", |b| {
        let fs = MemoryFileSystem::new();
        let data = vec![0xAB; 65536];
        b.iter(|| {
            let mut file = fs.create_file("/bench.txt").unwrap();
            file.write_all(&data).unwrap();
            file.set_size(black_box(1024)).unwrap();
            black_box(file);
            fs.remove_file("/bench.txt").unwrap();
        });
    });

    group.bench_function("local_fs_shrink", |b| {
        let temp_dir = tempfile::tempdir().unwrap();
        let fs = LocalFileSystem::new(temp_dir.path());
        let data = vec![0xAB; 65536];
        b.iter(|| {
            let mut file = fs.create_file("/bench.txt").unwrap();
            file.write_all(&data).unwrap();
            file.set_size(black_box(1024)).unwrap();
            black_box(file);
            fs.remove_file("/bench.txt").unwrap();
        });
    });

    group.finish();
}

// ============================================================================
// Directory Operations Benchmarks
// ============================================================================

fn bench_directory_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("directory_operations");

    group.bench_function("memory_fs_create_dir", |b| {
        let fs = MemoryFileSystem::new();
        let mut counter = 0;
        b.iter(|| {
            let path = format!("/dir_{}", counter);
            counter += 1;
            fs.create_directory(&path).unwrap();
            black_box(&path);
        });
    });

    group.bench_function("local_fs_create_dir", |b| {
        let temp_dir = tempfile::tempdir().unwrap();
        let fs = LocalFileSystem::new(temp_dir.path());
        let mut counter = 0;
        b.iter(|| {
            let path = format!("/dir_{}", counter);
            counter += 1;
            fs.create_directory(&path).unwrap();
            black_box(&path);
        });
    });

    group.bench_function("memory_fs_create_dir_all", |b| {
        let fs = MemoryFileSystem::new();
        let mut counter = 0;
        b.iter(|| {
            let path = format!("/a/b/c/d/{}", counter);
            counter += 1;
            fs.create_directory_all(&path).unwrap();
            black_box(&path);
        });
    });

    group.bench_function("local_fs_create_dir_all", |b| {
        let temp_dir = tempfile::tempdir().unwrap();
        let fs = LocalFileSystem::new(temp_dir.path());
        let mut counter = 0;
        b.iter(|| {
            let path = format!("/a/b/c/d/{}", counter);
            counter += 1;
            fs.create_directory_all(&path).unwrap();
            black_box(&path);
        });
    });

    group.finish();
}

// ============================================================================
// Metadata Operations Benchmarks
// ============================================================================

fn bench_metadata_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("metadata_operations");

    group.bench_function("memory_fs_exists", |b| {
        let fs = MemoryFileSystem::new();
        fs.create_file("/bench.txt").unwrap();

        b.iter(|| {
            black_box(fs.exists("/bench.txt").unwrap());
        });
    });

    group.bench_function("local_fs_exists", |b| {
        let temp_dir = tempfile::tempdir().unwrap();
        let fs = LocalFileSystem::new(temp_dir.path());
        fs.create_file("/bench.txt").unwrap();

        b.iter(|| {
            black_box(fs.exists("/bench.txt").unwrap());
        });
    });

    group.bench_function("memory_fs_filesize", |b| {
        let fs = MemoryFileSystem::new();
        let mut file = fs.create_file("/bench.txt").unwrap();
        file.write_all(&vec![0xAB; 1024]).unwrap();
        drop(file);

        b.iter(|| {
            black_box(fs.filesize("/bench.txt").unwrap());
        });
    });

    group.bench_function("local_fs_filesize", |b| {
        let temp_dir = tempfile::tempdir().unwrap();
        let fs = LocalFileSystem::new(temp_dir.path());
        let mut file = fs.create_file("/bench.txt").unwrap();
        file.write_all(&vec![0xAB; 1024]).unwrap();
        drop(file);

        b.iter(|| {
            black_box(fs.filesize("/bench.txt").unwrap());
        });
    });

    group.finish();
}

// ============================================================================
// Mixed Workload Benchmarks
// ============================================================================

fn bench_mixed_workload(c: &mut Criterion) {
    let mut group = c.benchmark_group("mixed_workload");

    group.bench_function("memory_fs_create_write_read_delete", |b| {
        let fs = MemoryFileSystem::new();
        let data = vec![0xAB; 4096];
        let mut counter = 0;

        b.iter(|| {
            let path = format!("/bench_{}.txt", counter);
            counter += 1;

            // Create and write
            let mut file = fs.create_file(&path).unwrap();
            file.write_all(&data).unwrap();
            drop(file);

            // Read
            let mut file = fs.open_file(&path).unwrap();
            let mut buffer = vec![0u8; 4096];
            file.read_exact(&mut buffer).unwrap();
            drop(file);

            // Delete
            fs.remove_file(&path).unwrap();

            black_box(buffer);
        });
    });

    group.bench_function("local_fs_create_write_read_delete", |b| {
        let temp_dir = tempfile::tempdir().unwrap();
        let fs = LocalFileSystem::new(temp_dir.path());
        let data = vec![0xAB; 4096];
        let mut counter = 0;

        b.iter(|| {
            let path = format!("/bench_{}.txt", counter);
            counter += 1;

            // Create and write
            let mut file = fs.create_file(&path).unwrap();
            file.write_all(&data).unwrap();
            drop(file);

            // Read
            let mut file = fs.open_file(&path).unwrap();
            let mut buffer = vec![0u8; 4096];
            file.read_exact(&mut buffer).unwrap();
            drop(file);

            // Delete
            fs.remove_file(&path).unwrap();

            black_box(buffer);
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_file_creation,
    bench_sequential_write,
    bench_sequential_read,
    bench_random_access,
    bench_seek_operations,
    bench_file_resize,
    bench_directory_operations,
    bench_metadata_operations,
    bench_mixed_workload,
);

criterion_main!(benches);
