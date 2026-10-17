use diesel::prelude::{ExpressionMethods, OptionalExtension, QueryDsl, SelectableHelper};
use diesel::{self, SqliteConnection};
use diesel_async::{AsyncConnection, RunQueryDsl, sync_connection_wrapper::SyncConnectionWrapper};

pub type Conn = SyncConnectionWrapper<SqliteConnection>;

pub trait db_func {
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
    async fn insert_file(
        &mut self,
        file_path: &str,
        sha256: &str,
        id: i32,
    ) -> Result<(), Box<dyn std::error::Error>>;
    async fn get_run_counter(
        &mut self,
    ) -> Result<Option<crate::db_model::RunCounter>, Box<dyn std::error::Error>>;
    async fn update_run_counter(
        &mut self,
        new_run_id: i32,
    ) -> Result<(), Box<dyn std::error::Error>>;
}

pub async fn establish_db_connection() -> Result<Conn, Box<dyn std::error::Error>> {
    let database_url = "rag-sync.db";
    let connection = Conn::establish(database_url).await?;
    Ok(connection)
}

impl db_func for Conn {
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
        id: i32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use crate::schema::files::dsl::{files, id as file_id, path, sha256};
        diesel::insert_into(files)
            .values((file_id.eq(id), path.eq(file_path), sha256.eq(sha256_value)))
            .execute(self)
            .await?;
        Ok(())
    }
}
