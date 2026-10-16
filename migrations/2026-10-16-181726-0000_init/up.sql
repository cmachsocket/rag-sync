CREATE TABLE files (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    path TEXT NOT NULL UNIQUE,
    md5 TEXT,
    size INTEGER NOT NULL DEFAULT 0,
    mtime INTEGER,
    status TEXT NOT NULL DEFAULT 'pending',
    error_msg TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX idx_files_md5 ON files (md5);
CREATE INDEX idx_files_status ON files (status);
