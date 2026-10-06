            CREATE TABLE IF NOT EXISTS work_items (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                title         TEXT NOT NULL CHECK (length(trim(title)) > 0),
                description   TEXT,
                status        TEXT NOT NULL CHECK (
                    status IN ('pending', 'active', 'waiting', 'blocked', 'done', 'cancelled', 'deleted')
                ),
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL,
                deleted_at    TEXT,
                purge_after   TEXT,
                CHECK (
                    (status = 'deleted' AND deleted_at IS NOT NULL AND purge_after IS NOT NULL)
                    OR
                    (status != 'deleted' AND deleted_at IS NULL AND purge_after IS NULL)
                )
            );

            CREATE TABLE IF NOT EXISTS history_entries (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                work_item_id  INTEGER NOT NULL REFERENCES work_items(id) ON DELETE CASCADE,
                kind          TEXT NOT NULL,
                actor         TEXT NOT NULL CHECK (length(trim(actor)) > 0),
                note          TEXT,
                occurred_at   TEXT NOT NULL,
                changes_json  TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_work_items_status_updated
                ON work_items(status, updated_at DESC);
            CREATE INDEX IF NOT EXISTS idx_work_items_purge_after
                ON work_items(purge_after) WHERE status = 'deleted';
            CREATE INDEX IF NOT EXISTS idx_history_work_item
                ON history_entries(work_item_id, id);

            PRAGMA user_version = 1;
