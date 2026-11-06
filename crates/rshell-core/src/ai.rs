//! AI connection metadata and chat requests. API keys are stored separately by the config worker.
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::Zeroizing;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatThread {
    pub id: Uuid,
    pub title: String,
    pub messages: Vec<ChatMessage>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatHistory {
    pub threads: Vec<ChatThread>,
    pub active_id: Option<Uuid>,
}

impl ChatHistory {
    fn validate(&self) -> Result<()> {
        let mut ids = std::collections::HashSet::new();
        if self.threads.len() > 20
            || self
                .active_id
                .is_some_and(|id| !self.threads.iter().any(|t| t.id == id))
            || self.threads.iter().any(|thread| {
                !ids.insert(thread.id)
                    || thread.title.len() > 128
                    || thread.messages.len() > 20
                    || thread.messages.iter().any(|m| {
                        !matches!(m.role.as_str(), "user" | "assistant") || m.content.len() > 32_000
                    })
            })
        {
            bail!("AI 对话历史格式无效或超过容量限制");
        }
        Ok(())
    }
}

#[cfg(any(windows, target_os = "macos"))]
const HISTORY_MAGIC: &[u8] = b"RSHELL-AI-HISTORY-1\0";
#[cfg(any(windows, target_os = "macos"))]
const HISTORY_ENTROPY: Uuid = Uuid::from_bytes([
    0x52, 0x53, 0x48, 0x45, 0x4c, 0x4c, 0x2d, 0x41, 0x49, 0x2d, 0x48, 0x49, 0x53, 0x54, 0x01, 0x00,
]);
#[cfg(any(windows, target_os = "macos"))]
const MAX_HISTORY_FILE: u64 = 8 * 1024 * 1024;

#[cfg(any(windows, target_os = "macos"))]
pub fn load_history(path: &std::path::Path) -> Result<ChatHistory> {
    use std::io::Read;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ChatHistory::default());
        }
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_HISTORY_FILE + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_HISTORY_FILE || !bytes.starts_with(HISTORY_MAGIC) {
        bail!("AI 对话历史文件格式无效");
    }
    let plaintext =
        crate::credentials::crypt(HISTORY_ENTROPY, &bytes[HISTORY_MAGIC.len()..], false)?;
    let history: ChatHistory = serde_json::from_slice(&plaintext)?;
    history.validate()?;
    Ok(history)
}

#[cfg(any(windows, target_os = "macos"))]
pub fn save_history(path: &std::path::Path, history: &ChatHistory) -> Result<()> {
    use std::io::Write;
    history.validate()?;
    let plaintext = Zeroizing::new(serde_json::to_vec(history)?);
    if plaintext.len() as u64 > MAX_HISTORY_FILE - 4096 {
        bail!("AI 对话历史超过 8 MiB");
    }
    let encrypted = crate::credentials::crypt(HISTORY_ENTROPY, &plaintext, true)?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("AI 对话历史目录无效"))?;
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!("ai-history-{}.tmp", Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(HISTORY_MAGIC)?;
        file.write_all(&encrypted)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

pub async fn chat(
    profile: &AiProfile,
    key: Zeroizing<String>,
    messages: &[ChatMessage],
) -> Result<String> {
    profile.validate()?;
    if messages.is_empty()
        || messages.iter().any(|m| {
            !matches!(m.role.as_str(), "system" | "user" | "assistant") || m.content.len() > 32_000
        })
    {
        bail!("对话内容无效或过长");
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let request = match profile.api_format {
        ApiFormat::OpenAi => client
            .post(format!("{}/chat/completions", profile.base_url.trim_end_matches('/')))
            .bearer_auth(key.as_str())
            .json(&serde_json::json!({"model": profile.model, "messages": messages, "stream": false})),
        ApiFormat::Anthropic => client
            .post(format!("{}/v1/messages", profile.agent_base_url()?))
            .header("x-api-key", key.as_str())
            .header("anthropic-version", "2023-06-01")
            .json(&serde_json::json!({"model":profile.model,"max_tokens":4096,"stream":false,
                "system":messages.iter().filter(|m| m.role == "system").map(|m| m.content.as_str()).collect::<Vec<_>>().join("\n"),
                "messages":messages.iter().filter(|m| m.role != "system").collect::<Vec<_>>()
            })),
    };
    let response = request
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("请求失败：{e}"))?;
    let status = response.status();
    let mut response = response;
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > 1_048_576 {
            bail!("模型响应超过 1 MiB");
        }
        body.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        let detail = String::from_utf8_lossy(&body);
        bail!(
            "模型请求失败（HTTP {status}）：{}",
            detail
                .replace(key.as_str(), "[已隐藏]")
                .chars()
                .take(500)
                .collect::<String>()
        );
    }
    let value: serde_json::Value = serde_json::from_slice(&body)?;
    let answer = match profile.api_format {
        ApiFormat::OpenAi => value["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        ApiFormat::Anthropic => value["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
    };
    if answer.trim().is_empty() {
        bail!("模型响应没有可显示的文本");
    }
    if answer.len() > 32_000 {
        bail!("模型回复过长，无法显示");
    }
    Ok(answer.to_owned())
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApiFormat {
    #[default]
    OpenAi,
    Anthropic,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiProfile {
    #[serde(default)]
    pub api_format: ApiFormat,
    pub id: Uuid,
    pub name: String,
    pub base_url: String,
    pub model: String,
    pub remember_key: bool,
}

impl Default for AiProfile {
    fn default() -> Self {
        Self {
            api_format: ApiFormat::OpenAi,
            id: Uuid::new_v4(),
            name: "新 AI 配置".into(),
            base_url: "https://api.openai.com/v1".into(),
            model: String::new(),
            remember_key: false,
        }
    }
}

impl AiProfile {
    pub fn agent_base_url(&self) -> Result<String> {
        self.validate()?;
        if self.api_format != ApiFormat::Anthropic {
            bail!(
                "Agent 需要 Anthropic Messages 接口；请在 AI 设置中选择服务商支持的 Anthropic 接口类型和地址，OpenAI Chat Completions 不能直接使用"
            );
        }
        let base = self.base_url.trim_end_matches('/');
        Ok(base.strip_suffix("/v1").unwrap_or(base).to_owned())
    }

    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty()
            || self.name.len() > 128
            || self.model.trim().is_empty()
            || self.model.len() > 256
        {
            bail!("名称和模型不能为空，名称最多 128 字节，模型最多 256 字节");
        }
        let endpoint = self
            .base_url
            .strip_prefix("https://")
            .or_else(|| self.base_url.strip_prefix("http://"));
        if self.base_url.len() > 2048
            || self
                .base_url
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
            || self.base_url.contains(['@', '?', '#', '\\'])
            || endpoint.is_none_or(|s| s.split('/').next().is_none_or(str::is_empty))
        {
            bail!("API 地址须为 HTTP(S) 地址，不能含账号、密钥、查询参数或片段");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AiSettings {
    #[serde(default)]
    pub claude: crate::claude_agent::Settings,
    pub profiles: Vec<AiProfile>,
    pub default_id: Option<Uuid>,
    #[serde(default)]
    pub blocked_commands: Vec<String>,
    #[serde(default = "default_chat_font_size")]
    pub chat_font_size: f32,
    #[serde(default)]
    pub chat_monospace: bool,
}

fn default_chat_font_size() -> f32 {
    12.0
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            profiles: Vec::new(),
            claude: crate::claude_agent::Settings::default(),
            default_id: None,
            blocked_commands: Vec::new(),
            chat_font_size: default_chat_font_size(),
            chat_monospace: false,
        }
    }
}

impl AiSettings {
    pub fn validate(&self) -> Result<()> {
        self.claude.validate()?;
        if self.blocked_commands.len() > 100
            || self.blocked_commands.iter().any(|command| {
                command.is_empty()
                    || command.len() > 64
                    || !command
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-')
            })
        {
            bail!("自定义黑名单最多 100 项，每项只允许 1–64 个英文字母、数字或连字符");
        }
        if !self.chat_font_size.is_finite() || !(10.0..=24.0).contains(&self.chat_font_size) {
            bail!("AI 对话字号须在 10 到 24 之间");
        }
        if self.profiles.len() > 100 {
            bail!("最多保存 100 个 AI 配置");
        }
        let mut ids = std::collections::HashSet::new();
        for profile in &self.profiles {
            profile.validate()?;
            if !ids.insert(profile.id) {
                bail!("AI 配置 ID 重复");
            }
        }
        if self.default_id.is_some_and(|id| !ids.contains(&id)) {
            bail!("默认 AI 配置不存在");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    #[test]
    fn chat_font_size_migrates_and_validates() {
        let mut settings: AiSettings =
            serde_json::from_str(r#"{"profiles":[],"default_id":null}"#).unwrap();
        assert_eq!(settings.chat_font_size, 12.0);
        assert!(settings.blocked_commands.is_empty());
        settings.chat_font_size = 11.0;
        let restored: AiSettings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored.chat_font_size, 11.0);
        for size in [9.0, 25.0, f32::NAN] {
            settings.chat_font_size = size;
            assert!(settings.validate().is_err());
        }
    }
    #[test]
    fn custom_blocked_commands_validate() {
        let mut settings = AiSettings {
            blocked_commands: vec!["curl".into()],
            ..Default::default()
        };
        assert!(settings.validate().is_ok());
        settings.blocked_commands = vec!["curl | sh".into()];
        assert!(settings.validate().is_err());
    }
    #[test]
    fn rejects_missing_fields_and_secret_bearing_urls() {
        let mut profile = AiProfile::default();
        assert!(profile.validate().is_err());
        profile.model = "local-model".into();
        assert!(profile.validate().is_ok());
        for url in [
            "file:///tmp",
            "https://",
            "https://user:secret@host/v1",
            "https://host?key=secret",
            "https://host/#secret",
            "https://bad host",
        ] {
            profile.base_url = url.into();
            assert!(profile.validate().is_err());
        }
        profile.base_url = "http://127.0.0.1:11434/v1".into();
        assert!(profile.validate().is_ok());
    }

    #[tokio::test]
    async fn chat_sends_messages_and_reads_reply_on_loopback() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let read = stream.read(&mut buffer).unwrap();
                request.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&request);
                if let Some((headers, body)) = text.split_once("\r\n\r\n") {
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|n| n.parse().ok())
                        })
                        .unwrap();
                    if body.len() >= length {
                        break;
                    }
                }
            }
            let text = String::from_utf8(request).unwrap();
            assert!(text.starts_with("POST /v1/chat/completions HTTP/1.1"));
            assert!(
                text.to_ascii_lowercase()
                    .contains("authorization: bearer test-key")
            );
            assert!(text.contains("\"content\":\"hello\""));
            let body = r#"{"choices":[{"message":{"content":"world"}}]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let profile = AiProfile {
            base_url: format!("http://{address}/v1"),
            model: "test-model".into(),
            ..AiProfile::default()
        };
        let answer = chat(
            &profile,
            Zeroizing::new("test-key".into()),
            &[ChatMessage {
                role: "user".into(),
                content: "hello".into(),
            }],
        )
        .await
        .unwrap();
        assert_eq!(answer, "world");
        server.join().unwrap();
    }

    #[test]
    fn legacy_profiles_remain_openai_and_agent_requires_anthropic() {
        let old = serde_json::json!({"id":Uuid::new_v4(),"name":"legacy","model":"custom","base_url":"https://provider.invalid/v1","remember_key":false});
        let mut profile: AiProfile = serde_json::from_value(old).unwrap();
        assert_eq!(profile.api_format, ApiFormat::OpenAi);
        assert!(profile.agent_base_url().is_err());
        profile.api_format = ApiFormat::Anthropic;
        assert_eq!(
            profile.agent_base_url().unwrap(),
            "https://provider.invalid"
        );
        profile.base_url = "https://provider.invalid/anthropic/v1/".into();
        assert_eq!(
            profile.agent_base_url().unwrap(),
            "https://provider.invalid/anthropic"
        );
    }

    #[tokio::test]
    async fn anthropic_chat_uses_profile_endpoint_key_model_and_message_format() {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut reader = tokio::io::BufReader::new(socket);
            let mut headers = String::new();
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).await.unwrap() > 0);
                headers.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            assert!(headers.starts_with("POST /gateway/v1/messages HTTP/1.1"));
            assert!(
                headers
                    .to_lowercase()
                    .contains("x-api-key: synthetic-profile-key")
            );
            assert!(
                headers
                    .to_lowercase()
                    .contains("anthropic-version: 2023-06-01")
            );
            let length: usize = headers
                .lines()
                .find_map(|l| {
                    l.to_lowercase()
                        .strip_prefix("content-length: ")
                        .map(str::to_owned)
                })
                .unwrap()
                .parse()
                .unwrap();
            let mut body = vec![0; length];
            reader.read_exact(&mut body).await.unwrap();
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["model"], "custom-claude");
            assert_eq!(body["system"], "instructions");
            assert_eq!(
                body["messages"],
                serde_json::json!([{"role":"user","content":"hello"}])
            );
            let response = r#"{"content":[{"type":"text","text":"world"}]}"#;
            reader.get_mut().write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
        });
        let result = chat(
            &AiProfile {
                api_format: ApiFormat::Anthropic,
                base_url: format!("http://{address}/gateway/v1"),
                model: "custom-claude".into(),
                ..Default::default()
            },
            Zeroizing::new("synthetic-profile-key".into()),
            &[
                ChatMessage {
                    role: "system".into(),
                    content: "instructions".into(),
                },
                ChatMessage {
                    role: "user".into(),
                    content: "hello".into(),
                },
            ],
        )
        .await
        .unwrap();
        assert_eq!(result, "world");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn cancelling_a_pending_chat_releases_the_request() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                .unwrap();
            let mut buffer = [0u8; 4096];
            assert!(stream.read(&mut buffer).unwrap() > 0);
            let _ = ready_tx.send(());
            loop {
                match stream.read(&mut buffer) {
                    Ok(0) => return true,
                    Ok(_) => {}
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::ConnectionReset
                                | std::io::ErrorKind::ConnectionAborted
                        ) =>
                    {
                        return true;
                    }
                    Err(_) => return false,
                }
            }
        });
        let task = tokio::spawn(async move {
            chat(
                &AiProfile {
                    base_url: format!("http://{address}/v1"),
                    model: "test-model".into(),
                    ..Default::default()
                },
                Zeroizing::new("test-key".into()),
                &[ChatMessage {
                    role: "user".into(),
                    content: "等待回复".into(),
                }],
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), ready_rx)
            .await
            .unwrap()
            .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        // Let the runtime finish closing the cancelled HTTP connection before joining its peer.
        let closed = tokio::task::spawn_blocking(move || server.join().unwrap())
            .await
            .unwrap();
        assert!(closed, "取消请求后连接仍未释放");
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    #[cfg_attr(target_os = "macos", ignore = "requires unlocked macOS Keychain")]
    fn encrypted_history_survives_replacement_without_plaintext() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/ai-history-tests");
        std::fs::create_dir_all(&root).unwrap();
        let temp = tempfile::tempdir_in(root).unwrap();
        let path = temp.path().join("ai-history.bin");
        let id = Uuid::new_v4();
        let mut history = ChatHistory {
            threads: vec![ChatThread {
                id,
                title: "测试".into(),
                messages: vec![ChatMessage {
                    role: "user".into(),
                    content: "敏感测试文本".into(),
                }],
            }],
            active_id: Some(id),
        };
        assert_eq!(load_history(&path).unwrap(), ChatHistory::default());
        save_history(&path, &history).unwrap();
        assert_eq!(load_history(&path).unwrap(), history);
        assert!(
            !std::fs::read(&path)
                .unwrap()
                .windows("敏感测试文本".len())
                .any(|s| s == "敏感测试文本".as_bytes())
        );
        history.threads[0].messages.push(ChatMessage {
            role: "assistant".into(),
            content: "回复".into(),
        });
        save_history(&path, &history).unwrap();
        assert_eq!(load_history(&path).unwrap(), history);
    }
}
