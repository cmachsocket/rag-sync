// src/models.rs
use diesel::prelude::*;
use crate::schema::files;
use crate::schema::run_counter;

#[derive(Queryable, Selectable, Debug)]
#[diesel(table_name = files)]
pub struct File {
    pub id: i32,
    pub path: String,
    pub sha256: String,
    pub rag_id: Option<String>,
    pub run_id: i32,
    /// 产物在 output_dir 里的相对路径（含别名目录）。
    /// 由产物相对路径算出来并落库之后，清理就不再依赖配置 ——
    /// 改名、换目录、把别名从配置里删掉，都还能定位到原来那个文件。
    /// NULL 表示这行是加列之前写入的：它按旧规则（无别名目录）定位产物。
    pub output_rel_path: Option<String>,
}

#[derive(Queryable, Selectable, Debug)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
#[diesel(table_name = run_counter)]
pub struct RunCounter{
    pub id: i32,
    pub current_run_id: i32,
}