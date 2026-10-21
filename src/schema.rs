// @generated automatically by Diesel CLI.

diesel::table! {
    files (id) {
        id -> Integer,
        path -> Text,
        sha256 -> Text,
        rag_id -> Nullable<Text>,
        run_id -> Integer,
        output_rel_path -> Nullable<Text>,
        uploaded_sha256 -> Nullable<Text>,
    }
}

diesel::table! {
    run_counter (id) {
        id -> Integer,
        current_run_id -> Integer,
    }
}

diesel::table! {
    joplin_state (id) {
        id -> Integer,
        cursor -> Text,
    }
}

diesel::table! {
    joplin_notes (id) {
        id -> Integer,
        item_id -> Text,
        item_name -> Text,
        upload_name -> Text,
        sha256 -> Text,
        rag_id -> Nullable<Text>,
        updated_time -> BigInt,
    }
}

diesel::allow_tables_to_appear_in_same_query!(files, run_counter, joplin_state, joplin_notes,);
