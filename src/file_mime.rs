
/// 扩展名 -> MIME 类型，取自 MinerU 自身的 `MIME_TYPE_BY_EXTENSION`
/// (`mineru/filetypes.py`)。不能用各语言标准库的对照表，它们对
/// `mhtml` / `mht` / `rst` / `tex` / `ofd` / `shtml` 给的值与服务端不一致。

pub const SUFFIX_MIME: [(&str, &str); 25] = [
    ("pdf", "application/pdf"),
    ("ofd", "application/ofd"),
    ("epub", "application/epub+zip"),
    ("doc", "application/msword"),
    (
        "docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ),
    ("ppt", "application/vnd.ms-powerpoint"),
    (
        "pptx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    ),
    ("xls", "application/vnd.ms-excel"),
    (
        "xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
    ("rtf", "application/rtf"),
    ("odt", "application/vnd.oasis.opendocument.text"),
    ("ods", "application/vnd.oasis.opendocument.spreadsheet"),
    ("odp", "application/vnd.oasis.opendocument.presentation"),
    ("html", "text/html"),
    ("htm", "text/html"),
    ("shtml", "text/html"),
    ("mhtml", "multipart/related"),
    ("mht", "multipart/related"),
    ("csv", "text/csv"),
    ("tsv", "text/tab-separated-values"),
    ("md", "text/markdown"),
    ("markdown", "text/markdown"),
    ("rst", "text/x-rst"),
    ("tex", "text/x-tex"),
    ("txt", "text/plain"),
];