-- SQLite 不支持直接删列（3.35 以下的版本），只能重建表。
-- 这一列可空、也没有索引依赖，回滚就是照原样建一张表再搬数据。
PRAGMA foreign_keys = OFF;

CREATE TABLE files_old (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    path TEXT NOT NULL UNIQUE,
    sha256 TEXT NOT NULL,
    rag_id TEXT,
    run_id INTEGER NOT NULL DEFAULT 0,
    output_rel_path TEXT
);

INSERT INTO files_old (id, path, sha256, rag_id, run_id)
SELECT id, path, sha256, rag_id, run_id FROM files;

DROP TABLE files;
ALTER TABLE files_old RENAME TO files;

CREATE INDEX idx_files_sha256 ON files (sha256);
CREATE INDEX idx_files_run_id ON files (run_id);

PRAGMA foreign_keys = ON;
