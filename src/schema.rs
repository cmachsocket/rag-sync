// @generated automatically by Diesel CLI.

diesel::table! {
    files (id) {
        id -> Integer,
        path -> Text,
        sha256 -> Text,
        rag_id -> Nullable<Text>,
        run_id -> Integer,
        output_rel_path -> Nullable<Text>,
    }
}

diesel::table! {
    run_counter (id) {
        id -> Integer,
        current_run_id -> Integer,
    }
}

diesel::allow_tables_to_appear_in_same_query!(files, run_counter,);
