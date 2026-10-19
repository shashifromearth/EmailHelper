//! Minimal Telegram Bot API client (sendMessage + getUpdates long polling).

use serde::Deserialize;
use serde_json::json;

use crate::error::{AppError, AppResult};

/// Telegram rejects messages longer than 4096 characters.
pub const MAX_MESSAGE_CHARS: usize = 4000;

#[derive(Clone)]
pub struct TelegramClient {
    http: reqwest::Client,
    base: String,
    pub chat_id: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Update {
    pub update_id: i64,
    #[serde(default)]
    pub message: Option<Message>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Message {
    pub message_id: i64,
    pub chat: Chat,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub reply_to_message: Option<Box<Message>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Chat {
    pub id: i64,
    #[serde(default)]
    pub first_name: Option<String>,
    #[serde(default)]
    pub last_name: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Deserialize)]
struct ApiResponse<T> {
    ok: bool,
    result: Option<T>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize)]
struct Sent {
    message_id: i64,
}

/// reqwest errors carry the request URL, which contains the bot token.
fn net(e: reqwest::Error) -> AppError {
    AppError::Http(e.without_url())
}

impl TelegramClient {
    pub fn new(http: reqwest::Client, api_base: &str, token: &str, chat_id: i64) -> Self {
        Self {
            http,
            base: format!("{}/bot{}", api_base.trim_end_matches('/'), token),
            chat_id,
        }
    }

    /// `getMe`: the bot's @username, proving the token works.
    pub async fn get_me(&self) -> AppResult<String> {
        #[derive(Deserialize)]
        struct Me {
            #[serde(default)]
            username: Option<String>,
        }
        let resp = self
            .http
            .get(format!("{}/getMe", self.base))
            .send()
            .await
            .map_err(|e| AppError::Other(format!("telegram getMe: {}", e.without_url())))?;
        let status = resp.status();
        let r: ApiResponse<Me> = resp
            .json()
            .await
            .map_err(|_| AppError::Other(format!("telegram getMe: HTTP {status}")))?;
        if !r.ok {
            return Err(AppError::Other(format!(
                "telegram getMe: {}",
                r.description.unwrap_or_else(|| status.to_string())
            )));
        }
        Ok(r.result.and_then(|m| m.username).unwrap_or_default())
    }

    /// Send a plain-text message; returns the Telegram message id.
    pub async fn send(&self, text: &str, reply_to: Option<i64>) -> AppResult<i64> {
        self.send_to(self.chat_id, text, reply_to).await
    }

    /// Send to a specific chat (one bot can serve several people's chats).
    pub async fn send_to(&self, chat_id: i64, text: &str, reply_to: Option<i64>) -> AppResult<i64> {
        let text: String = if text.chars().count() > MAX_MESSAGE_CHARS {
            let mut t: String = text.chars().take(MAX_MESSAGE_CHARS - 1).collect();
            t.push('…');
            t
        } else {
            text.to_string()
        };
        let mut body = json!({
            "chat_id": chat_id,
            "text": text,
            "disable_web_page_preview": true,
        });
        if let Some(r) = reply_to {
            body["reply_parameters"] =
                json!({ "message_id": r, "allow_sending_without_reply": true });
        }
        let resp: ApiResponse<Sent> = self
            .http
            .post(format!("{}/sendMessage", self.base))
            .json(&body)
            .send()
            .await
            .map_err(net)?
            .json()
            .await
            .map_err(net)?;
        if !resp.ok {
            return Err(AppError::other(format!(
                "telegram sendMessage failed: {}",
                resp.description.unwrap_or_default()
            )));
        }
        resp.result
            .map(|s| s.message_id)
            .ok_or_else(|| AppError::other("telegram sendMessage returned no result"))
    }

    /// Long-poll for updates (blocks up to `timeout_secs` server-side).
    pub async fn get_updates(&self, offset: i64, timeout_secs: u64) -> AppResult<Vec<Update>> {
        let resp: ApiResponse<Vec<Update>> = self
            .http
            .post(format!("{}/getUpdates", self.base))
            .timeout(std::time::Duration::from_secs(timeout_secs + 15))
            .json(&json!({
                "offset": offset,
                "timeout": timeout_secs,
                "allowed_updates": ["message"],
            }))
            .send()
            .await
            .map_err(net)?
            .json()
            .await
            .map_err(net)?;
        if !resp.ok {
            return Err(AppError::other(format!(
                "telegram getUpdates failed: {}",
                resp.description.unwrap_or_default()
            )));
        }
        Ok(resp.result.unwrap_or_default())
    }
}
