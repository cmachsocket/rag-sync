use serde::Deserialize;


/*

```json
{
  "access_token": "<JWT, 长度 199>",
  "token_type": "bearer",
  "auth_configured": true,
  "auth_mode": "enabled",
  "core_version": "1.5.7",
  "api_version": "0344"
}
```
只有 access_token 是必需的。其余字段带 serde(default)，
服务端增删字段不会让登录整体失效。
*/
#[derive(Deserialize)]
pub struct LoginResponse {
    pub access_token: String,
    #[serde(default)]
    pub token_type: String,
    #[serde(default)]
    pub auth_configured: bool,
    #[serde(default)]
    pub auth_mode: String,
    #[serde(default)]
    pub core_version: String,
    #[serde(default)]
    pub api_version: String,
}