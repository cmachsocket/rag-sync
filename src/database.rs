use diesel::prelude::{ExpressionMethods, OptionalExtension, QueryDsl, SelectableHelper};
use diesel::{self, SqliteConnection};
use diesel_async::{AsyncConnection, RunQueryDsl, sync_connection_wrapper::SyncConnectionWrapper};


pub type Conn = SyncConnectionWrapper<SqliteConnection>;

/// 方法都直接给 `Conn` 实现，trait 只是为了按职责分组。代价是拿不到 `Send` 边界。
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
    /// 记下产物相对路径。跟着 rag_id 一起写：两者描述的是同一份产物的同一个状态，
    /// 分开写就会出现「知识库里已经是新的，记录还指着旧的产物」。
    async fn update_output_rel_path(
        &mut self,
        file_path: &str,
        new_output_rel_path: &str,
    ) -> Result<(), Box<dyn std::error::Error>>;
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

/// 与 `establish_db_connection` 相同，但库路径可指定（测试要用临时库）。
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
        rel_path: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // 参数名不能叫 output_rel_path：会盖掉同名的列，`.eq()` 就套到 &str 上了
        use crate::schema::files::dsl::{files, output_rel_path, path, rag_id, run_id as file_run_id, sha256};
        // 不写 id：让它走 AUTOINCREMENT
        diesel::insert_into(files)
            .values((
                path.eq(file_path),
                sha256.eq(sha256_value),
                file_run_id.eq(run_id),
                rag_id.eq(file_rag_id),
                output_rel_path.eq(rel_path),
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
