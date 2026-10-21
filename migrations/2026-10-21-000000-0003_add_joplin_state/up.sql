-- Joplin 同步的状态：delta 游标 + 每篇笔记的同步记录。
-- 笔记正文不落盘，这里只存「哪篇笔记、内容指纹、知识库里的 doc_id」。
CREATE TABLE joplin_state (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    cursor TEXT NOT NULL DEFAULT ''
);

CREATE TABLE joplin_notes (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    item_id TEXT NOT NULL DEFAULT '',
    item_name TEXT NOT NULL UNIQUE,
    upload_name TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    rag_id TEXT,
    updated_time BIGINT NOT NULL DEFAULT 0
);

CREATE INDEX idx_joplin_notes_item_id ON joplin_notes (item_id);
