import json
import collections
import os

results = collections.defaultdict(list)

lints_to_fix = [
    'clippy::must_use_candidate',
    'clippy::missing_errors_doc',
    'clippy::missing_panics_doc',
    'missing_docs'
]
all_lints = set()

if os.path.exists('clippy_output.jsonl'):
    # Try reading with utf-16 encoding because of Windows redirect behavior
    try:
        f = open('clippy_output.jsonl', 'r', encoding='utf-16')
        lines = f.readlines()
        f.close()
    except:
        f = open('clippy_output.jsonl', 'r', encoding='utf-8')
        lines = f.readlines()
        f.close()

    for line in lines:
        try:
            data = json.loads(line)
            if data.get('reason') == 'compiler-message':
                msg = data.get('message', {})
                code_obj = msg.get('code')
                if code_obj:
                    lint_name = code_obj.get('code')
                    all_lints.add(lint_name)
                    if not lints_to_fix or lint_name in lints_to_fix:
                        spans = msg.get('spans', [])
                        if spans:
                            primary_span = next((s for s in spans if s.get('is_primary')), spans[0])
                            file_name = primary_span.get('file_name')
                            line_number = primary_span.get('line_start')
                            if file_name and line_number:
                                results[file_name].append({
                                    'lint': lint_name,
                                    'line': line_number,
                                    'message': msg.get('message')
                                })
        except Exception as e:
            pass

print("All Lints Found:")
for l in sorted(all_lints):
    print(f"  {l}")
print("-" * 20)

for file_name, errors in sorted(results.items()):
    print(f"File: {file_name}")
    for err in sorted(errors, key=lambda x: x['line']):
        print(f"  Line {err['line']}: [{err['lint']}] {err['message']}")
