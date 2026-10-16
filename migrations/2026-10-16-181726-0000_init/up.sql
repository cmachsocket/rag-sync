CREATE TABLE files (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    path TEXT NOT NULL UNIQUE,
    md5 TEXT NOT NULL,
    run_id INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_files_md5 ON files (md5);
CREATE INDEX idx_files_run_id ON files (run_id);

CREATE TABLE run_counter (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    current_run_id INTEGER NOT NULL DEFAULT 0
);

INSERT INTO run_counter (id, current_run_id) VALUES (1, 0);
