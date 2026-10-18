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
}

#[derive(Queryable, Selectable, Debug)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
#[diesel(table_name = run_counter)]
pub struct RunCounter{
    pub id: i32,
    pub current_run_id: i32,
}