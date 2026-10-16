use crate::db_model;
use crate::sync_config;
use crate::sync_folder;
use reqwest;

use core::task;
use std::path::Path;

async fn minerU_tackle(
    client: reqwest::Client,
    file: db_model::File,
    config: sync_config::SyncConfig,
) -> Result<String, Box<dyn std::error::Error>> {
    let uploads_api = "/v1/uploads";
    let file_api = "/content";
    let complete_api = "/complete";
    let parse_api = "/v1/file_parse";
    let status_api = "/v1/tasks/";
    let file_size = tokio::fs::metadata(&file.path).await?.len();
    let mime_type = sync_folder::mime_for(Path::new(&file.path));
    let upload_request = crate::json_model::FileUploadRequest {
        file_path: file.path.clone(),
        file_size,
        mime_type: mime_type.to_string(),
    };
    let resp = client
        .post(format!("{}{}", config.mineru_url, uploads_api))
        .json(&upload_request)
        .send()
        .await?;
    let upload_response: crate::json_model::FileUploadResponse =
        resp.error_for_status()?.json().await?;
    let file_id = upload_response.id.clone();
    let _ = client
        .put(format!(
            "{}{}/{}{}",
            config.mineru_url, uploads_api, file_id, file_api
        ))
        .header("Content-Type", upload_response.mime_type.clone())
        .body(tokio::fs::read(&file.path).await?)
        .send()
        .await?;
    loop {
        let resp = client
            .get(format!(
                "{}{}/{}{}",
                config.mineru_url, uploads_api, file_id, complete_api
            ))
            .send()
            .await?;
        let complete_response: crate::json_model::FilePutResponse =
            resp.error_for_status()?.json().await?;
        if complete_response.status == "completed" {
            println!("文件 {} 上传完成", file.path);
            break;
        } else {
            println!(
                "文件 {} 上传中，状态: {}",
                file.path, complete_response.status
            );
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    }
    let parse_request = crate::json_model::FileParseRequest {
        files: vec![crate::json_model::FileSource {
            source: crate::json_model::FileSourceType {
                file_id: Some(file_id.clone()),
                url: None,
                name: None,
                data: None,
                path: None,
                r#type: "file_id".to_string(),
            },
        }],
    };
    let resp: crate::json_model::FileParseResponse = client
        .post(format!("{}{}", config.mineru_url, parse_api))
        .json(&parse_request)
        .send()
        .await?
        .json()
        .await?;
    if resp.status == "success" {
        println!("文件 {} 解析成功", file.path);
        let content = resp.files
            .iter()
            .map(|f| {
                f.content
                    .as_ref()
                    .unwrap()
                    .get("markdown")
                    .unwrap()
                    .clone()
            })
            .next()
            .unwrap();
        return Ok(content);
    } else if resp.status == "running" {
        loop {
            let task_id = resp.task_id.clone();
            let resp = client
                .get(format!("{}{}/{}", config.mineru_url, status_api, task_id))
                .send()
                .await?;
            let status_response: crate::json_model::TaskStatusResponse =
                resp.error_for_status()?.json().await?;
            if status_response.completed == status_response.total {
                println!("文件 {} 解析完成", file.path);
                break;
            } else {
                println!(
                    "文件 {} 解析中，状态: {}/{}",
                    file.path, status_response.completed, status_response.total
                );
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
        //GET /v1/files/{output_files.markdown.file_id}/content   → 直接返回 markdown 原文
        //从逻辑上保证单一
        let markdown_file_id = resp.files
            .iter()
            .map(|f| {
                f.output_files
                    .as_ref()
                    .unwrap()
                    .get("markdown")
                    .unwrap()
                    .file_id
                    .clone()
            })
            .next()
            .unwrap();
        let resp = client
            .get(format!(
                "{}{}/{}{}",
                config.mineru_url, uploads_api, markdown_file_id, file_api
            ))
            .send()
            .await?;
        let content: String = resp.text().await?;
        return Ok(content);
    } else {
        println!("文件 {} 解析失败，状态: {}", file.path, resp.status);
    }
    Err(format!("文件 {} 解析失败，状态: {}", file.path, resp.status).into())
}
