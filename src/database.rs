use diesel::prelude::{ExpressionMethods, OptionalExtension, QueryDsl, SelectableHelper};
use diesel::{self, SqliteConnection};
use diesel_async::{AsyncConnection, RunQueryDsl, sync_connection_wrapper::SyncConnectionWrapper};

pub type Conn = SyncConnectionWrapper<SqliteConnection>;

/// 方法直接给 `Conn` 实现，trait 只用来按职责分组（代价是拿不到 `Send` 边界）。
#[allow(async_fn_in_trait)]
pub trait DBfunc {
    async fn file_exists(&mut self, file_path: &str) -> Result<bool, Box<dyn std::error::Error>>;
    async fn get_sha256(
        &mut self,
        file_path: &str,
    ) -> Result<Option<String>, Box<dyn std::error::Error>>;
    async fn update_sha256(
        &mut self,
        file_path: &str,
        new_sha256: &str,
    ) -> Result<(), Box<dyn std::error::Error>>;
    /// `id` 交给 AUTOINCREMENT，不接受外部指定（曾有调用方误传 run_id）。
    async fn insert_file(
        &mut self,
        file_path: &str,
        sha256: &str,
        run_id: i32,
        file_rag_id: Option<&str>,
        output_rel_path: &str,
        uploaded_sha256: &str,
    ) -> Result<(), Box<dyn std::error::Error>>;
    async fn delete_file(&mut self, file_path: &str) -> Result<(), Box<dyn std::error::Error>>;
    async fn get_run_counter(
        &mut self,
    ) -> Result<Option<crate::db_model::RunCounter>, Box<dyn std::error::Error>>;
    async fn update_run_counter(
        &mut self,
        new_run_id: i32,
    ) -> Result<(), Box<dyn std::error::Error>>;
    async fn get_file_run_id(
        &mut self,
        file_path: &str,
    ) -> Result<Option<i32>, Box<dyn std::error::Error>>;
    async fn update_file_run_id(
        &mut self,
        file_path: &str,
        new_run_id: i32,
    ) -> Result<(), Box<dyn std::error::Error>>;
    async fn get_file(
        &mut self,
        file_path: &str,
    ) -> Result<Option<crate::db_model::File>, Box<dyn std::error::Error>>;
    async fn update_rag_id(
        &mut self,
        file_path: &str,
        new_rag_id: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>>;
    /// 记下产物相对路径，跟着 rag_id 一起写：分开写会出现记录指着旧产物的状态。
    async fn update_output_rel_path(
        &mut self,
        file_path: &str,
        new_output_rel_path: &str,
    ) -> Result<(), Box<dyn std::error::Error>>;
    /// 记下 rag_id 那份文档的内容指纹，跟着 rag_id 一起写，对不上会跳过本该做的重传。
    /// 传 None 是作废：解析出新内容后旧指纹不再代表知识库里的那一份。
    async fn update_uploaded_sha256(
        &mut self,
        file_path: &str,
        new_uploaded_sha256: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>>;
    async fn found_file_less_than_run_id(
        &mut self,
        file_run_id: i32,
    ) -> Result<Vec<crate::db_model::File>, Box<dyn std::error::Error>>;
    /// delta 游标：没有记录就是「还没同步过，从头开始」。
    async fn get_joplin_cursor(&mut self) -> Result<Option<String>, Box<dyn std::error::Error>>;
    async fn set_joplin_cursor(&mut self, cursor: &str) -> Result<(), Box<dyn std::error::Error>>;
    /// 按服务端文件名取笔记记录（文件名是取内容与认领笔记的依据）。
    async fn get_joplin_note(
        &mut self,
        item_name: &str,
    ) -> Result<Option<crate::db_model::JoplinNote>, Box<dyn std::error::Error>>;
    /// 全部笔记记录，用于全量重放后清掉已经不存在的笔记。
    async fn list_joplin_notes(
        &mut self,
    ) -> Result<Vec<crate::db_model::JoplinNote>, Box<dyn std::error::Error>>;
    /// 记下「这篇笔记已经进了知识库」。按 item_name 覆盖写。
    async fn upsert_joplin_note(
        &mut self,
        item_id: &str,
        item_name: &str,
        upload_name: &str,
        sha256: &str,
        rag_id: Option<&str>,
        updated_time: i64,
    ) -> Result<(), Box<dyn std::error::Error>>;
    async fn delete_joplin_note(
        &mut self,
        item_name: &str,
    ) -> Result<(), Box<dyn std::error::Error>>;
    /// 清空笔记记录：游标失效要从头全量同步时用。
    async fn clear_joplin_notes(&mut self) -> Result<(), Box<dyn std::error::Error>>;
}

pub async fn establish_db_connection() -> Result<Conn, Box<dyn std::error::Error>> {
    let database_url = "rag-sync.db";
    let connection = Conn::establish(database_url).await?;
    Ok(connection)
}

/// 与 `establish_db_connection` 相同，但库路径可指定（测试要用临时库）。
pub async fn establish_db_connection_at(
    database_url: &str,
) -> Result<Conn, Box<dyn std::error::Error>> {
    let connection = Conn::establish(database_url).await?;
    Ok(connection)
}

impl DBfunc for Conn {
    async fn file_exists(&mut self, file_path: &str) -> Result<bool, Box<dyn std::error::Error>> {
        use crate::schema::files::dsl::*;
        let count = files
            .filter(path.eq(file_path))
            .count()
            .get_result::<i64>(self)
            .await?;
        Ok(count > 0)
    }
    async fn get_sha256(
        &mut self,
        file_path: &str,
    ) -> Result<Option<String>, Box<dyn std::error::Error>> {
        use crate::schema::files::dsl::*;
        let result = files
            .filter(path.eq(file_path))
            .select(sha256)
            .first::<String>(self)
            .await
            .optional()?;
        Ok(result)
    }
    async fn update_sha256(
        &mut self,
        file_path: &str,
        new_sha256: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use crate::schema::files::dsl::*;
        diesel::update(files.filter(path.eq(file_path)))
            .set(sha256.eq(new_sha256))
            .execute(self)
            .await?;
        Ok(())
    }
    async fn get_run_counter(
        &mut self,
    ) -> Result<Option<crate::db_model::RunCounter>, Box<dyn std::error::Error>> {
        use crate::db_model::RunCounter;
        use crate::schema::run_counter::dsl::run_counter;
        let result = run_counter
            .select(RunCounter::as_select())
            .first::<RunCounter>(self)
            .await
            .optional()?;
        Ok(result)
    }
    async fn update_run_counter(
        &mut self,
        new_run_id: i32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use crate::schema::run_counter::dsl::*;
        diesel::update(run_counter)
            .set(current_run_id.eq(new_run_id))
            .execute(self)
            .await?;
        Ok(())
    }
    async fn insert_file(
        &mut self,
        file_path: &str,
        sha256_value: &str,
        run_id: i32,
        file_rag_id: Option<&str>,
        rel_path: &str,
        uploaded_sha256: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // 参数名不能叫 output_rel_path：会盖掉同名的列，`.eq()` 就套到 &str 上了
        use crate::schema::files::dsl::{
            files, output_rel_path, path, rag_id, run_id as file_run_id, sha256,
            uploaded_sha256 as uploaded_sha256_col,
        };
        // 不写 id：让它走 AUTOINCREMENT
        diesel::insert_into(files)
            .values((
                path.eq(file_path),
                sha256.eq(sha256_value),
                file_run_id.eq(run_id),
                rag_id.eq(file_rag_id),
                output_rel_path.eq(rel_path),
                uploaded_sha256_col.eq(uploaded_sha256),
            ))
            .execute(self)
            .await?;
        Ok(())
    }
    async fn delete_file(&mut self, file_path: &str) -> Result<(), Box<dyn std::error::Error>> {
        use crate::schema::files::dsl::*;
        diesel::delete(files.filter(path.eq(file_path)))
            .execute(self)
            .await?;
        Ok(())
    }
    async fn get_file_run_id(
        &mut self,
        file_path: &str,
    ) -> Result<Option<i32>, Box<dyn std::error::Error>> {
        use crate::schema::files::dsl::*;
        let result = files
            .filter(path.eq(file_path))
            .select(run_id)
            .first::<i32>(self)
            .await
            .optional()?;
        Ok(result)
    }
    async fn update_file_run_id(
        &mut self,
        file_path: &str,
        new_run_id: i32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use crate::schema::files::dsl::*;
        diesel::update(files.filter(path.eq(file_path)))
            .set(run_id.eq(new_run_id))
            .execute(self)
            .await?;
        Ok(())
    }
    async fn get_file(
        &mut self,
        file_path: &str,
    ) -> Result<Option<crate::db_model::File>, Box<dyn std::error::Error>> {
        use crate::db_model::File;
        use crate::schema::files::dsl::*;
        let result = files
            .filter(path.eq(file_path))
            .select(File::as_select())
            .first::<File>(self)
            .await
            .optional()?;
        Ok(result)
    }
    async fn update_rag_id(
        &mut self,
        file_path: &str,
        new_rag_id: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use crate::schema::files::dsl::*;
        diesel::update(files.filter(path.eq(file_path)))
            .set(rag_id.eq(new_rag_id))
            .execute(self)
            .await?;
        Ok(())
    }
    async fn update_output_rel_path(
        &mut self,
        file_path: &str,
        new_output_rel_path: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use crate::schema::files::dsl::*;
        diesel::update(files.filter(path.eq(file_path)))
            .set(output_rel_path.eq(new_output_rel_path))
            .execute(self)
            .await?;
        Ok(())
    }
    async fn update_uploaded_sha256(
        &mut self,
        file_path: &str,
        new_uploaded_sha256: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use crate::schema::files::dsl::*;
        diesel::update(files.filter(path.eq(file_path)))
            .set(uploaded_sha256.eq(new_uploaded_sha256))
            .execute(self)
            .await?;
        Ok(())
    }
    async fn found_file_less_than_run_id(
        &mut self,
        file_run_id: i32,
    ) -> Result<Vec<crate::db_model::File>, Box<dyn std::error::Error>> {
        use crate::db_model::File;
        use crate::schema::files::dsl::*;
        let results = files
            .filter(run_id.lt(file_run_id))
            .select(File::as_select())
            .load::<File>(self)
            .await?;
        Ok(results)
    }

    async fn get_joplin_cursor(&mut self) -> Result<Option<String>, Box<dyn std::error::Error>> {
        use crate::schema::joplin_state::dsl::{cursor, joplin_state};
        let result = joplin_state
            .select(cursor)
            .first::<String>(self)
            .await
            .optional()?;
        Ok(result)
    }

    async fn set_joplin_cursor(
        &mut self,
        cursor_value: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use crate::schema::joplin_state::dsl::{cursor, joplin_state};
        // 单行表：没有就建一行，有就改它
        let updated = diesel::update(joplin_state)
            .set(cursor.eq(cursor_value))
            .execute(self)
            .await?;
        if updated == 0 {
            diesel::insert_into(joplin_state)
                .values(cursor.eq(cursor_value))
                .execute(self)
                .await?;
        }
        Ok(())
    }

    async fn get_joplin_note(
        &mut self,
        item_name_value: &str,
    ) -> Result<Option<crate::db_model::JoplinNote>, Box<dyn std::error::Error>> {
        use crate::db_model::JoplinNote;
        use crate::schema::joplin_notes::dsl::*;
        let result = joplin_notes
            .filter(item_name.eq(item_name_value))
            .select(JoplinNote::as_select())
            .first::<JoplinNote>(self)
            .await
            .optional()?;
        Ok(result)
    }

    async fn list_joplin_notes(
        &mut self,
    ) -> Result<Vec<crate::db_model::JoplinNote>, Box<dyn std::error::Error>> {
        use crate::db_model::JoplinNote;
        use crate::schema::joplin_notes::dsl::*;
        let results = joplin_notes
            .select(JoplinNote::as_select())
            .load::<JoplinNote>(self)
            .await?;
        Ok(results)
    }

    async fn upsert_joplin_note(
        &mut self,
        item_id_value: &str,
        item_name_value: &str,
        upload_name_value: &str,
        sha256_value: &str,
        rag_id_value: Option<&str>,
        updated_time_value: i64,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use crate::schema::joplin_notes::dsl::*;
        use diesel::upsert::excluded;
        diesel::insert_into(joplin_notes)
            .values((
                item_id.eq(item_id_value),
                item_name.eq(item_name_value),
                upload_name.eq(upload_name_value),
                sha256.eq(sha256_value),
                rag_id.eq(rag_id_value),
                updated_time.eq(updated_time_value),
            ))
            .on_conflict(item_name)
            .do_update()
            .set((
                item_id.eq(excluded(item_id)),
                upload_name.eq(excluded(upload_name)),
                sha256.eq(excluded(sha256)),
                rag_id.eq(excluded(rag_id)),
                updated_time.eq(excluded(updated_time)),
            ))
            .execute(self)
            .await?;
        Ok(())
    }

    async fn delete_joplin_note(
        &mut self,
        item_name_value: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use crate::schema::joplin_notes::dsl::*;
        diesel::delete(joplin_notes.filter(item_name.eq(item_name_value)))
            .execute(self)
            .await?;
        Ok(())
    }

    async fn clear_joplin_notes(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        use crate::schema::joplin_notes::dsl::*;
        diesel::delete(joplin_notes).execute(self).await?;
        Ok(())
    }
}
