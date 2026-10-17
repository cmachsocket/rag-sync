use serde::{Deserialize, Serialize};


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
*/
#[derive(Deserialize)]
pub struct LoginResponse {
    pub access_token: String,
    pub token_type: String,
    pub auth_configured: bool,
    pub auth_mode: String,
    pub core_version: String, 
    pub api_version: String,
}