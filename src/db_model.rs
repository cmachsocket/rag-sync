use crate::schema::files;
use crate::schema::joplin_notes;
use crate::schema::run_counter;
use diesel::prelude::*;

#[derive(Queryable, Selectable, Debug, Clone)]
#[diesel(table_name = files)]
pub struct File {
    pub id: i32,
    pub path: String,
    /// 源文件的 sha256，判「文件变了没有」用。
    pub sha256: String,
    pub rag_id: Option<String>,
    pub run_id: i32,
    /// 产物在 output_dir 里的相对路径（含别名目录）。落库后清理不再依赖配置。
    /// NULL 表示加列之前写入的行，按旧规则（无别名目录）定位产物。
    pub output_rel_path: Option<String>,
    /// **上传进知识库的产物正文**的 sha256，不是源文件的。
    /// 源文件变了不代表正文变了，靠这一列认出「知识库里已经是这份内容」，
    /// 免去 409 自愈的先删后传。NULL 表示还没成功传过，或是加列前写入的行。
    pub uploaded_sha256: Option<String>,
}

#[derive(Queryable, Selectable, Debug)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
#[diesel(table_name = run_counter)]
pub struct RunCounter {
    pub id: i32,
    pub current_run_id: i32,
}

/// 一篇 Joplin 笔记的同步状态。正文不落盘，这里只记「是哪篇、内容指纹、
/// 知识库里的 doc_id」。
#[derive(Queryable, Selectable, Debug, Clone)]
#[diesel(table_name = joplin_notes)]
pub struct JoplinNote {
    pub id: i32,
    /// Joplin 侧的 item id。老格式是 20 位，新格式是 32 位十六进制。
    pub item_id: String,
    /// 服务端上的文件名（`<id>.md`），取内容靠它。
    pub item_name: String,
    /// 传给 LightRAG 的文件名（`<别名>__<item_name>`）。
    pub upload_name: String,
    /// 笔记正文的 sha256，判「内容变了没有」用。
    pub sha256: String,
    /// 知识库里的 doc_id，删除时用。
    pub rag_id: Option<String>,
    /// 服务端记这条变更的时间（毫秒）。
    pub updated_time: i64,
}
