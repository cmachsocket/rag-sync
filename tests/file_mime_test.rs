//! 扩展名 -> MIME 映射。表照抄自 MinerU 服务端，抄错要到解析阶段才暴露，故固定整张表。

use rag_sync::file_mime::{mime_for, SUFFIX_MIME};
use rag_sync::sync_folder::is_supported_file;
use std::path::Path;

#[test]
fn suffix_table_has_25_unique_entries() {
    assert_eq!(SUFFIX_MIME.len(), 25);
    let mut seen = std::collections::HashSet::new();
    for (suffix, _) in SUFFIX_MIME.iter() {
        assert!(seen.insert(*suffix), "后缀重复: {suffix}");
    }
}

#[test]
fn suffixes_lowercase_and_mime_well_formed() {
    for (suffix, mime) in SUFFIX_MIME.iter() {
        assert_eq!(*suffix, suffix.to_ascii_lowercase(), "后缀必须小写: {suffix}");
        assert!(!suffix.is_empty());
        assert!(mime.contains('/'), "MIME 形状不对: {mime}");
    }
}

#[test]
fn common_suffixes_map_correctly() {
    let cases = [
        ("a.pdf", "application/pdf"),
        ("a.docx", "application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
        ("a.xlsx", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
        ("a.md", "text/markdown"),
        ("a.markdown", "text/markdown"),
        ("a.txt", "text/plain"),
        ("a.csv", "text/csv"),
        ("a.ofd", "application/ofd"),
        // 这几个是「标准库给的值与服务端不一致」的那批，容易被顺手改错
        ("a.mhtml", "multipart/related"),
        ("a.mht", "multipart/related"),
        ("a.rst", "text/x-rst"),
        ("a.tex", "text/x-tex"),
        ("a.shtml", "text/html"),
        ("a.epub", "application/epub+zip"),
    ];
    for (name, expected) in cases {
        assert_eq!(mime_for(Path::new(name)), expected, "{name}");
    }
}

#[test]
fn uppercase_suffix_still_matches() {
    assert_eq!(mime_for(Path::new("REPORT.PDF")), "application/pdf");
    assert_eq!(
        mime_for(Path::new("a.DoCx")),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
    );
}

#[test]
fn only_last_suffix_counts() {
    assert_eq!(mime_for(Path::new("归档.2024.final.pdf")), "application/pdf");
    assert_eq!(mime_for(Path::new("/tmp/a/b/c.tar.gz")), "application/octet-stream");
}

#[test]
fn unknown_suffix_falls_back_to_octet_stream() {
    assert_eq!(mime_for(Path::new("a.png")), "application/octet-stream");
    assert_eq!(mime_for(Path::new("a.exe")), "application/octet-stream");
    assert_eq!(mime_for(Path::new("noext")), "application/octet-stream");
    assert_eq!(mime_for(Path::new("")), "application/octet-stream");
    // 隐藏文件：Path::extension 对 ".gitignore" 返回 None
    assert_eq!(mime_for(Path::new(".gitignore")), "application/octet-stream");
}

#[test]
fn unicode_and_space_in_path_do_not_matter() {
    assert_eq!(mime_for(Path::new("/home/u/我的 文档/a.pdf")), "application/pdf");
}

#[tokio::test]
async fn supported_file_matches_mime_table() {
    for (suffix, _) in SUFFIX_MIME.iter() {
        let p = format!("/tmp/x.{suffix}");
        assert!(
            is_supported_file(Path::new(&p)).await,
            "{suffix} 在 MIME 表里，却没被 get_files 收进来"
        );
        // 与 mime_for 一样先转小写再比，所以 REPORT.PDF 也要被收
        assert!(
            is_supported_file(Path::new(&p.to_uppercase())).await,
            "{suffix} 大写时没被收进来，与 mime_for 的口径不一致"
        );
    }
    for name in ["/tmp/x.png", "/tmp/x", "/tmp/x.zip", "/tmp/x.exe"] {
        assert!(!is_supported_file(Path::new(name)).await, "{name} 不该被收");
    }
}
