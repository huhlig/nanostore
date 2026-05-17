#!/usr/bin/env python3
"""
Comprehensive test file fixer for MVCC transaction parameter changes.
Adds missing imports and fixes method signatures.
"""
import re
import os

def add_imports_if_missing(content, filepath):
    """Add TransactionId and LogSequenceNumber imports if missing."""
    needs_txn_id = 'TransactionId' in content and 'use nanokv::txn::' in content
    needs_lsn = 'LogSequenceNumber' in content and 'use nanokv::wal::' in content
    
    if needs_txn_id and 'TransactionId' not in content.split('use nanokv::txn::')[1].split(';')[0]:
        # Add TransactionId to txn imports
        content = re.sub(
            r'use nanokv::txn::\{([^}]+)\}',
            lambda m: f'use nanokv::txn::{{{m.group(1)}, TransactionId}}' if 'TransactionId' not in m.group(1) else m.group(0),
            content
        )
        print(f'  Added TransactionId import to {filepath}')
    
    if needs_lsn and 'LogSequenceNumber' not in content.split('use nanokv::wal::')[1].split(';')[0] if 'use nanokv::wal::' in content else True:
        # Add LogSequenceNumber import if wal is imported
        if 'use nanokv::wal::' in content:
            content = re.sub(
                r'use nanokv::wal::LogSequenceNumber;',
                r'use nanokv::wal::LogSequenceNumber;',
                content
            )
        else:
            # Add new import line after other nanokv imports
            insert_pos = content.rfind('use nanokv::')
            if insert_pos != -1:
                line_end = content.find('\n', insert_pos)
                content = content[:line_end+1] + 'use nanokv::wal::LogSequenceNumber;\n' + content[line_end+1:]
                print(f'  Added LogSequenceNumber import to {filepath}')
    
    return content

def fix_bloom_transaction_tests():
    """Fix bloom_transaction_tests.rs"""
    filepath = 'tests/bloom_transaction_tests.rs'
    print(f'Fixing {filepath}...')
    
    with open(filepath, 'r', encoding='utf-8') as f:
        content = f.read()
    
    # Add missing import
    if 'TransactionId' not in content.split('use')[0] if 'use' in content else True:
        # Find the imports section and add TransactionId
        content = re.sub(
            r'(use nanokv::vfs::MemoryFileSystem;)',
            r'\1\nuse nanokv::txn::TransactionId;',
            content
        )
    
    # Fix insert_key calls that are missing tx_id and commit_lsn
    # Pattern: bloom.insert_key(key)?
    content = re.sub(
        r'bloom\.insert_key\(([^,)]+)\)\?',
        r'bloom.insert_key(\1, TransactionId::from(1), LogSequenceNumber::from(1))?',
        content
    )
    
    # Pattern: txn.insert_key(key)
    content = re.sub(
        r'txn\.insert_key\(([^,)]+)\)',
        r'txn.insert_key(\1, TransactionId::from(1), LogSequenceNumber::from(1))',
        content
    )
    
    with open(filepath, 'w', encoding='utf-8', newline='\n') as f:
        f.write(content)
    print(f'  Fixed {filepath}')

def fix_fulltext_transaction_tests():
    """Fix fulltext_transaction_tests.rs"""
    filepath = 'tests/fulltext_transaction_tests.rs'
    if not os.path.exists(filepath):
        print(f'  Skipping {filepath} (not found)')
        return
        
    print(f'Fixing {filepath}...')
    
    with open(filepath, 'r', encoding='utf-8') as f:
        content = f.read()
    
    # Add missing imports
    if 'TransactionId' not in content:
        content = re.sub(
            r'(use nanokv::vfs::MemoryFileSystem;)',
            r'\1\nuse nanokv::txn::TransactionId;',
            content
        )
    
    if 'LogSequenceNumber' not in content:
        content = re.sub(
            r'(use nanokv::txn::TransactionId;)',
            r'\1\nuse nanokv::wal::LogSequenceNumber;',
            content
        )
    
    # Fix index_document calls
    content = re.sub(
        r'FullTextSearch::index_document\(([^,]+),\s*([^,]+),\s*([^)]+)\)',
        r'FullTextSearch::index_document(\1, \2, \3, TransactionId::from(1), LogSequenceNumber::from(1))',
        content
    )
    
    with open(filepath, 'w', encoding='utf-8', newline='\n') as f:
        f.write(content)
    print(f'  Fixed {filepath}')

def fix_rtree_geospatial_tests():
    """Fix rtree_geospatial_tests.rs"""
    filepath = 'tests/rtree_geospatial_tests.rs'
    if not os.path.exists(filepath):
        print(f'  Skipping {filepath} (not found)')
        return
        
    print(f'Fixing {filepath}...')
    
    with open(filepath, 'r', encoding='utf-8') as f:
        content = f.read()
    
    # Fix insert_geometry calls missing commit_lsn
    # Pattern: .insert_geometry(..., TransactionId::from(n)),
    content = re.sub(
        r'(\.insert_geometry\([^)]*TransactionId::from\(\d+\)),\s*\)',
        r'\1, LogSequenceNumber::from(1))',
        content,
        flags=re.DOTALL
    )
    
    with open(filepath, 'w', encoding='utf-8', newline='\n') as f:
        f.write(content)
    print(f'  Fixed {filepath}')

def fix_table_index_tests():
    """Fix table_index_tests.rs"""
    filepath = 'tests/table_index_tests.rs'
    if not os.path.exists(filepath):
        print(f'  Skipping {filepath} (not found)')
        return
        
    print(f'Fixing {filepath}...')
    
    with open(filepath, 'r', encoding='utf-8') as f:
        content = f.read()
    
    # Fix insert_key
    content = re.sub(
        r'bloom\.insert_key\(([^)]+)\)\.unwrap\(\)',
        r'bloom.insert_key(\1, TransactionId::from(1), LogSequenceNumber::from(1)).unwrap()',
        content
    )
    
    # Fix insert_vector
    content = re.sub(
        r'hnsw\.insert_vector\(([^,]+),\s*([^)]+)\)',
        r'hnsw.insert_vector(\1, \2, TransactionId::from(1), LogSequenceNumber::from(1))',
        content
    )
    
    # Fix insert_geometry
    content = re.sub(
        r'(\.insert_geometry\([^)]*TransactionId::from\(\d+\)),\s*\)',
        r'\1, LogSequenceNumber::from(1))',
        content,
        flags=re.DOTALL
    )
    
    with open(filepath, 'w', encoding='utf-8', newline='\n') as f:
        f.write(content)
    print(f'  Fixed {filepath}')

def main():
    print('Starting comprehensive test file fixes...\n')
    
    fix_bloom_transaction_tests()
    fix_fulltext_transaction_tests()
    fix_rtree_geospatial_tests()
    fix_table_index_tests()
    
    print('\nAll fixes applied successfully!')

if __name__ == '__main__':
    main()

# Made with Bob
