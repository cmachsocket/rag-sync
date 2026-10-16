// src/models.rs
use diesel::prelude::*;
use crate::schema::files;

#[derive(Queryable, Selectable, Debug)]
#[diesel(table_name = files)]
pub struct File {
    pub id: i32,
    pub path: String,
    pub sha256: String,
}

#[derive(Queryable, Debug)]
#[diesel(table_name = files)]
pub struct RunCounter{
    pub id: Option<i32>,
    pub current_run_id: i32,
}