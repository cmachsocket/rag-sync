// @generated automatically by Diesel CLI.

diesel::table! {
    files (id) {
        id -> Nullable<Integer>,
        path -> Text,
        md5 -> Nullable<Text>,
        size -> Integer,
        mtime -> Nullable<Integer>,
        status -> Text,
        error_msg -> Nullable<Text>,
        created_at -> Integer,
        updated_at -> Integer,
    }
}
