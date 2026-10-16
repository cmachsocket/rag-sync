// @generated automatically by Diesel CLI.

diesel::table! {
    files (id) {
        id -> Integer,
        path -> Text,
        md5 -> Text,
        run_id -> Integer,
    }
}

diesel::table! {
    run_counter (id) {
        id -> Nullable<Integer>,
        current_run_id -> Integer,
    }
}

diesel::allow_tables_to_appear_in_same_query!(files, run_counter,);
