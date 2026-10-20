use crate::schema::files;
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
