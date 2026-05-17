#!/usr/bin/env python3
"""
Fix remaining test compilation errors for MVCC transaction parameters.
"""
import re

def fix_bloom_transaction_tests():
    """Fix remaining issues in bloom_transaction_tests.rs"""
    filepath = 'tests/bloom_transaction_tests.rs'
    print(f'Fixing {filepath}...')
    
    with open(filepath, 'r', encoding='utf-8') as f:
        content = f.read()
    
    # Fix insert_key calls without ? that were missed
    # Pattern: bloom.insert_key(arg)?; -> already fixed
    # Pattern: bloom.insert_key(arg); -> needs fixing
    content = re.sub(
        r'bloom\.insert_key\(([^,)]+)\);',
        r'bloom.insert_key(\1, TransactionId::from(1), LogSequenceNumber::from(1));',
        content
    )
    
    # Fix might_contain calls in closures that return Result
    # These don't need tx_id/commit_lsn, they're read operations
    
    with open(filepath, 'w', encoding='utf-8', newline='\n') as f:
        f.write(content)
    print(f'  Fixed {filepath}')

def fix_rtree_geospatial_tests():
    """Fix remaining issues in rtree_geospatial_tests.rs"""
    filepath = 'tests/rtree_geospatial_tests.rs'
    print(f'Fixing {filepath}...')
    
    with open(filepath, 'r', encoding='utf-8') as f:
        content = f.read()
    
    # Fix insert_geometry calls that span multiple lines
    # Look for patterns like:
    # .insert_geometry(
    #     ...
    #     TransactionId::from(1),
    # )
    # Should be:
    # .insert_geometry(
    #     ...
    #     TransactionId::from(1),
    #     LogSequenceNumber::from(1),
    # )
    
    # More aggressive pattern to catch multi-line cases
    content = re.sub(
        r'(\.insert_geometry\([^)]*TransactionId::from\(\d+\)\s*,)\s*\)',
        r'\1\n            LogSequenceNumber::from(1),\n        )',
        content,
        flags=re.DOTALL
    )
    
    with open(filepath, 'w', encoding='utf-8', newline='\n') as f:
        f.write(content)
    print(f'  Fixed {filepath}')

def fix_table_index_tests():
    """Fix remaining issues in table_index_tests.rs"""
    filepath = 'tests/table_index_tests.rs'
    print(f'Fixing {filepath}...')
    
    with open(filepath, 'r', encoding='utf-8') as f:
        content = f.read()
    
    # Fix insert_geometry calls
    content = re.sub(
        r'(\.insert_geometry\([^)]*TransactionId::from\(\d+\)\s*,)\s*\)',
        r'\1\n            LogSequenceNumber::from(1),\n        )',
        content,
        flags=re.DOTALL
    )
    
    # Fix GeoPoint::new calls that now take extra parameters
    # GeoPoint::new(lat, lon) -> just GeoPoint::new(lat, lon)
    # The issue is that insert_geometry is being called with wrong number of args
    
    with open(filepath, 'w', encoding='utf-8', newline='\n') as f:
        f.write(content)
    print(f'  Fixed {filepath}')

def fix_fulltext_transaction_tests():
    """Fix remaining issues in fulltext_transaction_tests.rs"""
    filepath = 'tests/fulltext_transaction_tests.rs'
    print(f'Fixing {filepath}...')
    
    with open(filepath, 'r', encoding='utf-8') as f:
        content = f.read()
    
    # The script already added the parameters, but there might be malformed ones
    # Look for patterns like: TransactionId::from(1, TransactionId::from(1), ...)
    # This suggests the regex matched incorrectly
    
    # Fix any double-application of the fix
    content = re.sub(
        r'TransactionId::from\(1, TransactionId::from\(1\), LogSequenceNumber::from\(1\)\), LogSequenceNumber::from\(1\)',
        r'TransactionId::from(1), LogSequenceNumber::from(1)',
        content
    )
    
    with open(filepath, 'w', encoding='utf-8', newline='\n') as f:
        f.write(content)
    print(f'  Fixed {filepath}')

def main():
    print('Fixing remaining test compilation errors...\n')
    
    fix_bloom_transaction_tests()
    fix_rtree_geospatial_tests()
    fix_table_index_tests()
    fix_fulltext_transaction_tests()
    
    print('\nAll remaining fixes applied!')

if __name__ == '__main__':
    main()

# Made with Bob
