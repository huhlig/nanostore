import re

# Fix rtree_geospatial_tests.rs
with open('tests/rtree_geospatial_tests.rs', 'r', encoding='utf-8') as f:
    content = f.read()

# Fix multi-line insert_geometry - add LogSequenceNumber after TransactionId
content = re.sub(
    r'(\.insert_geometry\([^)]*TransactionId::from\(\d+\)),\s*\)',
    r'\1, LogSequenceNumber::from(1))',
    content, flags=re.DOTALL
)

with open('tests/rtree_geospatial_tests.rs', 'w', encoding='utf-8', newline='\n') as f:
    f.write(content)
print('Fixed rtree_geospatial_tests.rs')

# Fix table_index_tests.rs
with open('tests/table_index_tests.rs', 'r', encoding='utf-8') as f:
    content = f.read()

# Fix insert_key
content = re.sub(
    r'bloom\.insert_key\(key\.as_bytes\(\)\)\.unwrap\(\)',
    r'bloom.insert_key(key.as_bytes(), TransactionId::from(1), LogSequenceNumber::from(1)).unwrap()',
    content
)

# Fix insert_vector
content = content.replace(
    'hnsw.insert_vector(b"id1", &[1.0, 0.0, 0.0])',
    'hnsw.insert_vector(b"id1", &[1.0, 0.0, 0.0], TransactionId::from(1), LogSequenceNumber::from(1))'
)

# Fix multi-line insert_geometry
content = re.sub(
    r'(\.insert_geometry\([^)]*TransactionId::from\(\d+\)),\s*\)',
    r'\1, LogSequenceNumber::from(1))',
    content, flags=re.DOTALL
)

with open('tests/table_index_tests.rs', 'w', encoding='utf-8', newline='\n') as f:
    f.write(content)
print('Fixed table_index_tests.rs')

# Fix bloom_transaction_tests.rs
with open('tests/bloom_transaction_tests.rs', 'r', encoding='utf-8') as f:
    content = f.read()

# Fix all remaining insert_key calls
content = re.sub(
    r'bloom\.insert_key\(([^)]+)\)\?',
    r'bloom.insert_key(\1, TransactionId::from(1), LogSequenceNumber::from(1))?',
    content
)

content = re.sub(
    r'txn\.insert_key\(([^)]+)\)',
    r'txn.insert_key(\1, TransactionId::from(1), LogSequenceNumber::from(1))',
    content
)

with open('tests/bloom_transaction_tests.rs', 'w', encoding='utf-8', newline='\n') as f:
    f.write(content)
print('Fixed bloom_transaction_tests.rs')

# Fix fulltext_transaction_tests.rs
with open('tests/fulltext_transaction_tests.rs', 'r', encoding='utf-8') as f:
    content = f.read()

# Fix FullTextSearch trait method
content = content.replace(
    'FullTextSearch::index_document(&mut txn, b"doc1", &[])',
    'FullTextSearch::index_document(&mut txn, b"doc1", &[], TransactionId::from(1), LogSequenceNumber::from(1))'
)

with open('tests/fulltext_transaction_tests.rs', 'w', encoding='utf-8', newline='\n') as f:
    f.write(content)
print('Fixed fulltext_transaction_tests.rs')

print('All fixes applied')

# Made with Bob
