//! JumpServer Community v4.10.2 API adapter. No target-host credentials are retrieved.
use anyhow::{Context, Result, bail, ensure};
use reqwest::{Method, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashSet, time::Duration};
use uuid::Uuid;
use zeroize::Zeroizing;

#[cfg(test)]
#[path = "jumpserver_tests.rs"]
mod tests;

#[cfg(windows)]
#[path = "jumpserver_credentials.rs"]
pub mod credentials;

#[cfg(target_os = "macos")]
#[path = "jumpserver_credentials_macos.rs"]
pub mod credentials;

const MAX_RESPONSE: usize = 4 * 1024 * 1024;
const MAX_ITEMS: usize = 20_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Saved {
    pub settings: Settings,
    pub snapshot: Option<Snapshot>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Settings {
    pub id: Uuid,
    pub name: String,
    pub url: String,
    pub ssh_host: String,
    pub ssh_port: u16,
    pub username: String,
    pub org_id: String,
    pub remember: bool,
    pub remember_password: bool,
    pub keep_minutes: u32,
    pub default_accounts: [String; 3],
    pub auto_sync: bool,
    pub sync_interval_minutes: u32,
    pub allow_http: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            name: "JumpServer".into(),
            url: String::new(),
            ssh_host: String::new(),
            ssh_port: 2222,
            username: String::new(),
            org_id: String::new(),
            remember: false,
            remember_password: false,
            auto_sync: true,
            sync_interval_minutes: 60,
            allow_http: false,
            keep_minutes: 480,
            default_accounts: Default::default(),
        }
    }
}

impl Settings {
    pub fn sync_interval(&self) -> Duration {
        Duration::from_secs(u64::from(self.sync_interval_minutes.clamp(1, 43200)) * 60)
    }

    pub fn validate(&self) -> Result<Url> {
        ensure!(
            (1..=43200).contains(&self.sync_interval_minutes),
            "资产刷新间隔必须在 1 分钟至 30 天之间"
        );
        let mut url =
            Url::parse(self.url.trim()).context("请输入完整的 JumpServer Web / API 地址")?;
        ensure!(
            url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "Web 地址不得包含凭据、查询参数或片段"
        );
        ensure!(
            url.scheme() == "https" || (url.scheme() == "http" && self.allow_http),
            "请使用 HTTPS；仅 HTTP 部署需明确勾选允许明文 HTTP"
        );
        ensure!(
            url.path() == "/" || url.path().is_empty(),
            "请输入站点根地址，不包含 /ui 或 API 路径"
        );
        url.set_path("/");
        ensure!(
            !self.name.trim().is_empty() && self.name.len() <= 128,
            "实例名称不能为空或超过 128 字节"
        );
        ensure!(
            !self.ssh_host.trim().is_empty()
                && self.ssh_host.len() <= 253
                && !self
                    .ssh_host
                    .chars()
                    .any(|c| c.is_whitespace() || "/@?#\\".contains(c))
                && self.ssh_port > 0,
            "请输入有效的 KoKo SSH 主机与端口"
        );
        ensure!(
            !self.username.trim().is_empty() && self.username.len() <= 256,
            "请输入登录用户名"
        );
        ensure!(
            self.org_id.is_empty() || Uuid::parse_str(&self.org_id).is_ok(),
            "组织 ID 无效"
        );
        ensure!(
            (1..=43200).contains(&self.keep_minutes),
            "登录保持时间必须在 1 分钟至 30 天之间"
        );
        ensure!(
            self.default_accounts
                .iter()
                .all(|a| a.len() <= 128 && !a.chars().any(char::is_control)),
            "默认账号不能超过 128 字节或包含控制字符"
        );
        Ok(url)
    }

    /// Any change of authority invalidates authentication and cached permissions.
    pub fn same_identity(&self, other: &Self) -> bool {
        self.id == other.id
            && self.url == other.url
            && self.username == other.username
            && self.org_id == other.org_id
            && self.ssh_host == other.ssh_host
            && self.ssh_port == other.ssh_port
            && self.allow_http == other.allow_http
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Organization {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Node {
    pub id: Uuid,
    pub name: String,
    pub key: String,
    #[serde(default)]
    pub full_value: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Asset {
    pub id: Uuid,
    pub name: String,
    pub address: String,
    #[serde(default, deserialize_with = "deserialize_asset_nodes")]
    pub nodes: Vec<Uuid>,
    #[serde(default = "yes")]
    pub is_active: bool,
}

fn deserialize_asset_nodes<'de, D>(deserializer: D) -> std::result::Result<Vec<Uuid>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // v4.10.2 ObjectRelatedField emits {id, name}; existing local snapshots
    // store UUID strings. Preserve the same strict UUID identity for both.
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum NodeRef {
        Id(Uuid),
        Object { id: Uuid },
    }
    Vec::<NodeRef>::deserialize(deserializer).map(|nodes| {
        nodes
            .into_iter()
            .map(|node| match node {
                NodeRef::Id(id) | NodeRef::Object { id } => id,
            })
            .collect()
    })
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Snapshot {
    pub user_id: String,
    pub org_id: String,
    pub timestamp: u64,
    pub nodes: Vec<Node>,
    pub assets: Vec<Asset>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Account {
    pub alias: String,
    pub name: String,
    pub username: String,
    pub has_secret: bool,
    pub has_username: bool,
    pub actions: Value,
}
impl Account {
    pub fn can_connect(&self) -> bool {
        self.actions.as_array().is_some_and(|a| {
            a.iter().any(|v| {
                v.get("value").and_then(Value::as_str) == Some("connect")
                    || v.as_str() == Some("connect")
            })
        })
    }
}

/// Only exact, unambiguous matches among the current permitted accounts may auto-select.
pub fn preferred_account(accounts: &[Account], priorities: &[String; 3]) -> Option<usize> {
    if accounts.len() == 1 {
        return Some(0);
    }
    for wanted in priorities
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        let matches: Vec<_> = accounts
            .iter()
            .enumerate()
            .filter(|(_, a)| a.username == wanted || a.alias == wanted)
            .map(|(i, _)| i)
            .collect();
        match matches.as_slice() {
            [index] => return Some(*index),
            [] => {}
            _ => return None,
        }
    }
    None
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn parse_expiry(value: &str) -> Result<u64> {
    // v4.10.2 apps/jumpserver/settings/libs.py uses strftime
    // %Y/%m/%d %H:%M:%S %z, rather than DRF's usual ISO-8601 default.
    let format = time::format_description::parse_borrowed::<2>(
        "[year]/[month]/[day] [hour]:[minute]:[second] [offset_hour sign:mandatory][offset_minute]",
    )?;
    let parsed = time::OffsetDateTime::parse(value, &format)
        .or_else(|_| {
            time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        })
        .context("登录有效期格式不兼容")?;
    u64::try_from(parsed.unix_timestamp()).context("登录有效期早于 Unix 时间起点")
}

#[derive(Serialize, Deserialize)]
struct SavedLogin {
    token: String,
    expires_at: u64,
    authenticated_at: u64,
    url: String,
    username: String,
    ssh_host: String,
    ssh_port: u16,
}
impl Drop for SavedLogin {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.token.zeroize();
    }
}

#[derive(Clone, Debug)]
pub enum AuthStep {
    LoggedIn,
    Mfa(Vec<String>),
    Approval,
}

pub struct Resolved {
    pub asset: Asset,
    pub account: String,
    pub username: String,
    pub secret: Zeroizing<String>,
}

/// Clone shares only the HTTP cookie jar; a GUI generation owns all in-flight requests.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: Url,
    pub settings: Settings,
    token: Option<Zeroizing<String>>,
    expires_at: u64,
    authenticated_at: u64,
    pub user_id: String,
    pub username: String,
    pub organizations: Vec<Organization>,
}

impl Client {
    pub fn new(settings: Settings) -> Result<Self> {
        let base = settings.validate()?;
        let http = reqwest::Client::builder()
            .cookie_store(true)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .user_agent("RShell/JumpServer-v4.10.2")
            .build()?;
        Ok(Self {
            http,
            base,
            settings,
            token: None,
            expires_at: 0,
            authenticated_at: 0,
            user_id: String::new(),
            username: String::new(),
            organizations: vec![],
        })
    }

    pub fn logged_in(&self) -> bool {
        self.token.is_some() && now() < self.expires_at
    }
    pub fn expired(&self) -> bool {
        self.token.is_some() && !self.logged_in()
    }
    pub fn remaining_seconds(&self) -> u64 {
        self.expires_at.saturating_sub(now())
    }
    pub fn shorten_session(&mut self) {
        self.expires_at = self.expires_at.min(
            self.authenticated_at
                .saturating_add(u64::from(self.settings.keep_minutes) * 60),
        );
    }
    pub fn saved_token(&self) -> Option<Zeroizing<String>> {
        self.token
            .as_ref()
            .filter(|_| self.logged_in())
            .map(|token| {
                Zeroizing::new(
                    serde_json::to_string(&SavedLogin {
                        token: token.to_string(),
                        expires_at: self.expires_at,
                        authenticated_at: self.authenticated_at,
                        url: self.settings.url.clone(),
                        username: self.settings.username.clone(),
                        ssh_host: self.settings.ssh_host.clone(),
                        ssh_port: self.settings.ssh_port,
                    })
                    .expect("serializable login record"),
                )
            })
    }

    async fn request(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        ensure!(!self.expired(), "登录保持时间已到，请重新登录并完成 MFA");
        ensure!(
            path.starts_with("api/v1/") && !path.contains(".."),
            "无效 API 路径"
        );
        let url = self.base.join(path)?;
        let mut request = self
            .http
            .request(method, url)
            .header("Accept", "application/json");
        if let Some(token) = &self.token {
            request = request.bearer_auth(token.as_str());
        }
        if !self.settings.org_id.is_empty() {
            request = request.header("X-JMS-ORG", &self.settings.org_id);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let mut response = request
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("JumpServer 网络请求失败（检查地址、证书和网络）"))?;
        let status = response.status();
        ensure!(
            !status.is_redirection(),
            "JumpServer 返回重定向，请配置实际 API 根地址"
        );
        ensure!(
            response.content_length().unwrap_or(0) <= MAX_RESPONSE as u64,
            "JumpServer 响应超过容量限制"
        );
        let mut bytes = Zeroizing::new(Vec::new());
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow::anyhow!("JumpServer 响应读取失败"))?
        {
            ensure!(
                bytes.len() + chunk.len() <= MAX_RESPONSE,
                "JumpServer 响应超过容量限制"
            );
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).map_err(|_| {
                anyhow::anyhow!("JumpServer 返回非预期 JSON（HTTP {}）", status.as_u16())
            })?
        };
        if !status.is_success() {
            // Never expose arbitrary server bodies: they can echo credentials.
            let code = value
                .get("code")
                .or_else(|| value.get("error"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let hint = match code {
                "acl_review" => "需要资产连接审批，请先在 JumpServer 完成审批",
                "acl_reject" => "资产连接被服务端 ACL 拒绝",
                "acl_face_verify" | "acl_face_online_not_supported" => {
                    "此资产要求网页人脸验证，请使用 JumpServer 网页流程"
                }
                "perm_expired" | "perm_account_invalid" => "资产账号权限已失效",
                "session_empty" => "认证挑战已过期，请重新登录",
                _ if status.as_u16() == 401 => "登录已失效或验证码错误，请重新认证",
                _ if status.as_u16() == 403 => "服务端拒绝此操作，请检查当前用户权限",
                _ => "请求未通过，请检查账号、验证码或服务端配置",
            };
            bail!("{hint}（HTTP {}）", status.as_u16());
        }
        Ok(value)
    }

    pub async fn login(&mut self, password: &str) -> Result<AuthStep> {
        let response = self
            .request(
                Method::POST,
                "api/v1/authentication/auth/",
                Some(json!({
                    "username": self.settings.username, "password": password
                })),
            )
            .await?;
        self.accept_auth(response).await
    }

    pub async fn continue_login(&mut self) -> Result<AuthStep> {
        let response = self
            .request(Method::POST, "api/v1/authentication/auth/", Some(json!({})))
            .await?;
        self.accept_auth(response).await
    }

    pub async fn verify_mfa(&mut self, kind: &str, code: &str) -> Result<AuthStep> {
        ensure!(!code.is_empty() && code.len() <= 256, "请输入有效验证码");
        self.request(
            Method::POST,
            "api/v1/authentication/mfa/challenge/",
            Some(json!({"type":kind,"code":code})),
        )
        .await?;
        self.continue_login().await
    }

    pub async fn send_mfa(&self, kind: &str) -> Result<()> {
        self.request(
            Method::POST,
            "api/v1/authentication/mfa/send-code/",
            Some(json!({"type":kind})),
        )
        .await?;
        Ok(())
    }

    async fn accept_auth(&mut self, response: Value) -> Result<AuthStep> {
        match response.get("error").and_then(Value::as_str) {
            Some("mfa_required") => {
                let choices = response
                    .pointer("/data/choices")
                    .and_then(Value::as_array)
                    .context("MFA 类型响应不完整")?;
                let choices: Vec<_> = choices
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|kind| {
                        matches!(*kind, "otp" | "sms" | "email" | "otp_radius" | "mfa_custom")
                    })
                    .take(16)
                    .map(str::to_owned)
                    .collect();
                ensure!(
                    !choices.is_empty(),
                    "服务端未提供验证码型 MFA；人脸或 Passkey 请使用 JumpServer 网页认证"
                );
                Ok(AuthStep::Mfa(choices))
            }
            Some("login_confirm_wait") => Ok(AuthStep::Approval),
            Some(_) => bail!("服务端未完成登录，请检查认证或审批状态"),
            None => {
                let token = response
                    .get("token")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty() && s.len() <= 4096)
                    .context("登录响应缺少有效令牌")?;
                let expiry = response
                    .get("date_expired")
                    .and_then(Value::as_str)
                    .context("登录响应缺少有效期")?;
                let server_expiry = parse_expiry(expiry)?;
                self.authenticated_at = now();
                self.expires_at = server_expiry
                    .min(now().saturating_add(u64::from(self.settings.keep_minutes) * 60));
                ensure!(self.expires_at > now(), "服务端登录令牌已经过期");
                self.token = Some(Zeroizing::new(token.to_owned()));
                if let Err(error) = self.load_identity().await {
                    self.token = None;
                    return Err(error);
                }
                Ok(AuthStep::LoggedIn)
            }
        }
    }

    pub async fn restore(&mut self, token: Zeroizing<String>) -> Result<()> {
        ensure!(
            !token.is_empty() && token.len() <= 2560,
            "保存的登录状态无效"
        );
        let mut saved: SavedLogin =
            serde_json::from_str(&token).context("保存的登录状态格式无效")?;
        ensure!(
            saved.url == self.settings.url
                && saved.username == self.settings.username
                && saved.ssh_host == self.settings.ssh_host
                && saved.ssh_port == self.settings.ssh_port,
            "保存的登录状态与当前实例不一致"
        );
        self.authenticated_at = saved.authenticated_at;
        self.expires_at = saved.expires_at;
        self.shorten_session();
        ensure!(
            self.expires_at > now() && self.authenticated_at <= now(),
            "保存的登录状态已过期，请重新登录并完成 MFA"
        );
        self.token = Some(Zeroizing::new(std::mem::take(&mut saved.token)));
        if let Err(error) = self.load_identity().await {
            self.token = None;
            return Err(error);
        }
        Ok(())
    }

    async fn load_identity(&mut self) -> Result<()> {
        let value = self
            .request(Method::GET, "api/v1/users/profile/permissions/", None)
            .await?;
        self.user_id = value
            .get("id")
            .and_then(Value::as_str)
            .context("用户 ID 缺失")?
            .to_owned();
        self.username = value
            .get("username")
            .and_then(Value::as_str)
            .context("用户名缺失")?
            .to_owned();
        ensure!(
            self.username == self.settings.username,
            "登录身份与配置用户名不一致"
        );
        self.organizations = serde_json::from_value(
            value
                .get("workbench_orgs")
                .cloned()
                .context("组织列表缺失")?,
        )
        .context("组织列表格式错误")?;
        ensure!(
            !self.organizations.is_empty() && self.organizations.len() <= 256,
            "没有可用组织或组织数量超限"
        );
        if self.settings.org_id.is_empty() && self.organizations.len() == 1 {
            self.settings.org_id = self.organizations[0].id.clone();
        }
        Ok(())
    }

    async fn list<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<Vec<T>> {
        let mut result = Vec::new();
        let mut offset = 0;
        loop {
            let value = self
                .request(
                    Method::GET,
                    &format!("{path}?limit=200&offset={offset}"),
                    None,
                )
                .await?;
            let (items, more) = if let Some(items) = value.as_array() {
                (items, false)
            } else {
                let items = value
                    .get("results")
                    .and_then(Value::as_array)
                    .context("分页响应缺少 results")?;
                (items, value.get("next").is_some_and(|n| !n.is_null()))
            };
            ensure!(
                result.len() + items.len() <= MAX_ITEMS,
                "授权列表超过 20000 项限制，未替换缓存"
            );
            for item in items {
                result.push(
                    serde_json::from_value(item.clone())
                        .context("授权列表数据格式不兼容 v4.10.2")?,
                );
            }
            if !more {
                break;
            }
            ensure!(!items.is_empty(), "分页无进展，未替换缓存");
            // Reconstruct same-origin pagination. Never follow a server-supplied URL with credentials.
            offset += items.len();
        }
        Ok(result)
    }

    pub async fn sync(&self) -> Result<Snapshot> {
        ensure!(self.logged_in(), "请先登录 JumpServer");
        ensure!(
            self.organizations
                .iter()
                .any(|o| o.id == self.settings.org_id),
            "请选择有权限的组织"
        );
        let raw_nodes: Vec<Value> = self.list("api/v1/perms/users/self/nodes/").await?;
        // v4.10.2 PermNode adds virtual categories with non-UUID IDs. They are
        // not asset-node relations: render real directories and our ungrouped
        // fallback from the full authorized asset list, not empty virtual nodes.
        let nodes: Vec<Node> = raw_nodes
            .into_iter()
            .filter(|node| {
                !matches!(
                    node.get("id").and_then(Value::as_str),
                    Some("favorite" | "ungrouped")
                )
            })
            .map(|node| serde_json::from_value(node).context("授权节点数据格式不兼容 v4.10.2"))
            .collect::<Result<_>>()?;
        let assets: Vec<Asset> = self.list("api/v1/perms/users/self/assets/").await?;
        ensure!(
            nodes.iter().map(|n| n.id).collect::<HashSet<_>>().len() == nodes.len()
                && assets.iter().map(|a| a.id).collect::<HashSet<_>>().len() == assets.len(),
            "分页存在重复条目，请重新同步"
        );
        ensure!(
            nodes
                .iter()
                .all(|n| n.key.split(':').count() <= 64 && n.name.len() <= 1024)
                && assets
                    .iter()
                    .all(|a| a.name.len() <= 1024 && a.address.len() <= 1024),
            "资产或节点字段超过容量限制"
        );
        Ok(Snapshot {
            user_id: self.user_id.clone(),
            org_id: self.settings.org_id.clone(),
            nodes,
            assets,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs(),
        })
    }

    /// v4.10.2 exposes permitted accounts on asset details, not a global self-account route.
    /// Aggregate names only; candidates never grant authorization to another asset.
    pub async fn default_account_candidates(&self) -> Result<Vec<String>> {
        ensure!(self.logged_in(), "请先登录 JumpServer");
        ensure!(
            self.organizations
                .iter()
                .any(|o| o.id == self.settings.org_id),
            "请选择有权限的组织"
        );
        let assets: Vec<Asset> = self.list("api/v1/perms/users/self/assets/").await?;
        let mut candidates = std::collections::BTreeSet::new();
        let mut visited = HashSet::new();
        for asset in assets.into_iter().filter(|a| a.is_active) {
            if !visited.insert(asset.id) {
                continue;
            }
            let value = self
                .request(
                    Method::GET,
                    &format!("api/v1/perms/users/self/assets/{}/", asset.id),
                    None,
                )
                .await?;
            let protocols = value
                .get("permed_protocols")
                .and_then(Value::as_array)
                .context("授权协议列表缺失")?;
            if !protocols
                .iter()
                .any(|p| p.get("name").and_then(Value::as_str) == Some("ssh"))
            {
                continue;
            }
            let accounts: Vec<Account> = serde_json::from_value(
                value
                    .get("permed_accounts")
                    .cloned()
                    .context("授权账号列表缺失")?,
            )?;
            ensure!(accounts.len() <= 256, "授权账号数量超过限制");
            for account in accounts.into_iter().filter(Account::can_connect) {
                for name in [account.username, account.alias] {
                    if name.is_empty() {
                        continue;
                    }
                    ensure!(
                        name.len() <= 128 && !name.chars().any(char::is_control),
                        "授权账号名称无效"
                    );
                    candidates.insert(name);
                    ensure!(candidates.len() <= MAX_ITEMS, "默认账号候选数量超过限制");
                }
            }
        }
        Ok(candidates.into_iter().collect())
    }

    pub async fn accounts(&self, asset: Uuid) -> Result<Vec<Account>> {
        let value = self
            .request(
                Method::GET,
                &format!("api/v1/perms/users/self/assets/{asset}/"),
                None,
            )
            .await?;
        ensure!(
            value
                .get("permed_protocols")
                .and_then(Value::as_array)
                .is_some_and(|p| p
                    .iter()
                    .any(|p| p.get("name").and_then(Value::as_str) == Some("ssh"))),
            "此资产未授权 SSH 协议"
        );
        let mut accounts: Vec<Account> = serde_json::from_value(
            value
                .get("permed_accounts")
                .cloned()
                .context("授权账号列表缺失")?,
        )?;
        ensure!(accounts.len() <= 256, "授权账号数量超过限制");
        accounts.retain(Account::can_connect);
        ensure!(!accounts.is_empty(), "此资产没有可连接的授权账号");
        Ok(accounts)
    }

    pub async fn resolve(
        &self,
        asset: Asset,
        account: Account,
        input_username: &str,
        input_secret: &str,
    ) -> Result<Resolved> {
        // Recheck current permissions, never trust cached account metadata.
        let current = self.accounts(asset.id).await?;
        let account = current
            .into_iter()
            .find(|a| a.alias == account.alias)
            .context("账号授权已改变，请重新选择")?;
        ensure!(
            account.has_username || !input_username.is_empty(),
            "此账号需要输入目标用户名"
        );
        ensure!(
            account.has_secret || !input_secret.is_empty(),
            "此账号需要输入本次连接密码"
        );
        let value = self.request(Method::POST, "api/v1/authentication/connection-token/", Some(json!({
            "asset":asset.id,"account":account.alias,"protocol":"ssh","connect_method":"ssh_client",
            "input_username":input_username,"input_secret":input_secret,"is_reusable":false
        }))).await?;
        ensure!(
            value.get("is_active").and_then(Value::as_bool) == Some(true),
            "连接尚未授权，请在 JumpServer 完成审批或二次认证"
        );
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .context("连接令牌 ID 缺失")?;
        Uuid::parse_str(id).context("连接令牌 ID 无效")?;
        let secret = value
            .get("value")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && s.len() <= 4096)
            .context("连接令牌缺失")?;
        // v4.10.2 KoKo pkg/auth/ssh.go: JMS-{id} + token.value password.
        // Use only the explicitly configured KoKo endpoint, never asset.address.
        Ok(Resolved {
            asset,
            account: if account.has_username {
                account.username
            } else {
                input_username.to_owned()
            },
            username: format!("JMS-{id}"),
            secret: Zeroizing::new(secret.to_owned()),
        })
    }
}
