use diesel::prelude::{ExpressionMethods, OptionalExtension, QueryDsl, SelectableHelper};
use diesel::{self, SqliteConnection};
use diesel_async::{AsyncConnection, RunQueryDsl, sync_connection_wrapper::SyncConnectionWrapper};


pub type Conn = SyncConnectionWrapper<SqliteConnection>;

/// 所有方法都直接给 `Conn` 实现。trait 里用 `async fn` 只是为了按职责分组，
/// 代价是拿不到 `Send` 边界（`async fn` in trait 的固有限制，
/// 编译器会给 11 条 warning）。将来真要 `tokio::spawn` 包一层时，
/// 改成 `fn x(&mut self) -> impl Future<Output = ...> + Send` 即可。
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
    /// `id` 交给 AUTOINCREMENT，不接受外部指定。
    /// 曾经这里有一个 `id: i32` 形参，调用方传的是 `run_id`，
    /// 结果轮次号写进了主键列、run_id 停在 0 —— 详见 tests/known_issues_test.rs。
    async fn insert_file(
        &mut self,
        file_path: &str,
        sha256: &str,
        run_id: i32,
        file_rag_id: Option<&str>,
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
    ) -> Result<(), Box<dyn std::error::Error>> ;
    async fn found_file_less_than_run_id(
        &mut self,
        file_run_id: i32,
    ) -> Result<Vec<crate::db_model::File>, Box<dyn std::error::Error>>;
}

pub async fn establish_db_connection() -> Result<Conn, Box<dyn std::error::Error>> {
    let database_url = "rag-sync.db";
    let connection = Conn::establish(database_url).await?;
    Ok(connection)
}

/// 与 `establish_db_connection` 相同，但库路径可指定。测试要用临时库，
/// 否则每个用例都会打到仓库里那个真的 rag-sync.db 上。
pub async fn establish_db_connection_at(database_url: &str) -> Result<Conn, Box<dyn std::error::Error>> {
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
    ) -> Result<(), Box<dyn std::error::Error>> {
        use crate::schema::files::dsl::{files, path, rag_id, run_id as file_run_id, sha256};
        // 显式列出四个字段，不写 id：让它走 AUTOINCREMENT。
        diesel::insert_into(files)
            .values((
                path.eq(file_path),
                sha256.eq(sha256_value),
                file_run_id.eq(run_id),
                rag_id.eq(file_rag_id),
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
    async fn found_file_less_than_run_id(
        &mut self,
        file_run_id: i32,
    ) -> Result<Vec<crate::db_model::File>, Box<dyn std::error::Error>>
    {
        use crate::db_model::File;
        use crate::schema::files::dsl::*;
        let results = files
            .filter(run_id.lt(file_run_id))
            .select(File::as_select())
            .load::<File>(self)
            .await?;
        Ok(results)
    }
}
