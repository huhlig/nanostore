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

use nanokv::kvdb::Database;
use nanokv::table::{AppendLogConfig, TableEngineKind, TableOptions};
use nanokv::types::KeyEncoding;
use nanokv::vfs::MemoryFileSystem;

fn appendlog_table_options() -> TableOptions {
    let mut options = TableOptions::default();
    options.engine = TableEngineKind::AppendLog;
    options.key_encoding = KeyEncoding::RawBytes;
    options.appendlog_config = Some(AppendLogConfig::default());
    options
}

#[test]
fn test_appendlog_table_reopens_with_persisted_rows() {
    let fs = MemoryFileSystem::new();
    let table_id;
    let expected_root_page;

    {
        let db = Database::new(&fs, "appendlog-persist.wal", "appendlog-persist.db").unwrap();
        table_id = db
            .create_table("events", appendlog_table_options())
            .unwrap();

        let table = db.table(table_id).unwrap();
        table.insert(b"k1", b"v1").unwrap();
        table.insert(b"k2", b"v2").unwrap();

        expected_root_page = db
            .get_object_info(table_id)
            .unwrap()
            .unwrap()
            .root
            .unwrap()
            .page_id;
    }

    {
        let reopened = Database::open(&fs, "appendlog-persist.wal", "appendlog-persist.db").unwrap();
        let reopened_id = reopened.open_table("events").unwrap().unwrap();
        assert_eq!(reopened_id, table_id);

        let table = reopened.table(reopened_id).unwrap();

        assert_eq!(
            reopened
                .get_object_info(reopened_id)
                .unwrap()
                .unwrap()
                .root
                .unwrap()
                .page_id,
            expected_root_page
        );
        assert_eq!(
            table.get(b"k1").unwrap().map(|value| value.as_ref().to_vec()),
            Some(b"v1".to_vec())
        );
        assert_eq!(
            table.get(b"k2").unwrap().map(|value| value.as_ref().to_vec()),
            Some(b"v2".to_vec())
        );

        let info = reopened.get_object_info(reopened_id).unwrap().unwrap();
        assert_eq!(info.options.engine, appendlog_table_options().engine);
    }
}

// Made with Bob
