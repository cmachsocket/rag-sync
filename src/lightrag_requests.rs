use crate::lightrag_model;
use reqwest;

struct RAGRequest<'a> {
    client: &'a reqwest::Client,
    url: &'a str,
    jwt_token: String,
    username: &'a str,
    password: &'a str,
}

impl<'a> RAGRequest<'a> {
    pub async fn new(client: &'a reqwest::Client, url: &'a str, username: &'a str, password: &'a str) -> Self {
        RAGRequest {
            client,
            url,
            jwt_token: match Self::login(&client, &url, &username, &password).await {
                Ok(token) => token,
                Err(_) => String::new(),
            },
            username,
            password,
        }
    }
    pub async fn login(
        client: &reqwest::Client,
        url: &str,
        username: &str,
        password: &str,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let login_url = format!("{}/login", url);
        // grant_type=password&username=<user>&password=<pwd>
        let login_body = format!(
            r#"grant_type=password&username={}&password={}"#,
            username, password
        );

        let response = client
            .post(&login_url)
            .body(login_body)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send()
            .await?;

        if response.status().is_success() {
            let login_response: lightrag_model::LoginResponse = response.json().await?;
            Ok(login_response.access_token)
        } else {
            Err(format!("登录失败，状态码: {}", response.status()).into())
        }
    }
}
