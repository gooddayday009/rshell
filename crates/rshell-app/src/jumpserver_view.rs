use super::*;
use rshell_core::jumpserver::{
    self as jms, Account, Asset, AuthStep, Client, Saved, Settings, Snapshot,
};

#[derive(Clone)]
pub(super) struct SessionRef {
    pub settings: Settings,
    pub user_id: String,
    pub asset: Asset,
    pub account: String,
}

pub(super) struct State {
    pub open: bool,
    pub draft: Settings,
    settings: Settings,
    client: Option<Client>,
    snapshot: Option<Snapshot>,
    password: Zeroizing<String>,
    code: Zeroizing<String>,
    mfa: Vec<String>,
    mfa_kind: String,
    mfa_focus: bool,
    approval: bool,
    status: String,
    error: String,
    pub generation: Uuid,
    job: Option<tokio::sync::oneshot::Receiver<Reply>>,
    abort: Option<tokio::task::AbortHandle>,
    next_sync: Instant,
    restore: bool,
    pending_reconnect: Option<SessionRef>,
    reconnect_account: Option<String>,
    reconnect_inflight: Option<SessionRef>,
    reconnect_queue: std::collections::VecDeque<SessionRef>,
    auto_auth_blocked: bool,
    choice: Option<(Asset, Vec<Account>, Option<usize>)>,
    preference_accounts: Vec<String>,
    input_username: String,
    input_secret: Zeroizing<String>,
    pub recent: Vec<Asset>,
}

enum Work {
    Login(Zeroizing<String>),
    Restore(Zeroizing<String>),
    SavedPassword,
    Mfa(String, Zeroizing<String>),
    SendMfa(String),
    Continue,
    Sync,
    Accounts(Asset),
    PreferenceAccounts,
    Connect(Asset, Account, String, Zeroizing<String>),
}
enum Payload {
    Auth(AuthStep),
    Sent,
    Snapshot(Snapshot),
    Accounts(Asset, Vec<Account>),
    PreferenceAccounts(Vec<String>),
    Connected(jms::Resolved),
}
struct Reply {
    client: Client,
    restoring: bool,
    result: Result<Payload, String>,
}

impl State {
    pub fn new(saved: Option<&Saved>) -> Self {
        let settings = saved.map(|s| s.settings.clone()).unwrap_or_default();
        let next_sync = Instant::now() + settings.sync_interval();
        let smoke_mfa = std::env::args().any(|a| a == "--smoke-test")
            && std::env::args().any(|a| a == "--smoke-jumpserver-mfa");
        Self {
            open: std::env::args()
                .any(|a| a == "--smoke-jumpserver" || a == "--jumpserver-settings"),
            draft: settings.clone(),
            restore: saved.is_some_and(|s| s.settings.remember || s.settings.remember_password),
            pending_reconnect: None,
            reconnect_account: None,
            reconnect_inflight: None,
            reconnect_queue: Default::default(),
            auto_auth_blocked: false,
            settings,
            client: None,
            snapshot: saved.and_then(|s| s.snapshot.clone()),
            password: Zeroizing::new(String::new()),
            code: Zeroizing::new(String::new()),
            mfa: if smoke_mfa {
                vec!["otp".into()]
            } else {
                vec![]
            },
            mfa_kind: if smoke_mfa {
                "otp".into()
            } else {
                String::new()
            },
            mfa_focus: smoke_mfa,
            approval: false,
            status: "未登录；缓存仅供查看".into(),
            error: String::new(),
            generation: Uuid::new_v4(),
            job: None,
            abort: None,
            next_sync,
            choice: None,
            preference_accounts: vec![],
            input_username: String::new(),
            input_secret: Zeroizing::new(String::new()),
            recent: vec![],
        }
    }
    pub fn modal_open(&self) -> bool {
        self.open || self.choice.is_some() || !self.mfa.is_empty()
    }
    fn authenticated(&self) -> bool {
        self.client.as_ref().is_some_and(Client::logged_in)
    }
    fn busy(&self) -> bool {
        self.job.is_some()
    }
    fn cancel(&mut self) {
        if let Some(abort) = self.abort.take() {
            abort.abort();
        }
        self.job = None;
        self.generation = Uuid::new_v4();
        self.choice = None;
        self.pending_reconnect = None;
        self.reconnect_account = None;
        self.reconnect_inflight = None;
        self.reconnect_queue.clear();
        self.preference_accounts.clear();
        self.input_secret.zeroize();
        self.code.zeroize();
        self.mfa.clear();
        self.mfa_focus = false;
    }
    fn logout(&mut self) {
        self.cancel();
        self.client = None;
        self.mfa.clear();
        self.approval = false;
        self.snapshot = None;
        self.recent.clear();
        self.password.zeroize();
        self.restore = false;
        self.auto_auth_blocked = true;
        self.status = "已退出登录；已有终端可单独断开".into();
    }
    fn saved(&self) -> Saved {
        Saved {
            settings: self.settings.clone(),
            snapshot: self.snapshot.clone(),
        }
    }
    fn start(&mut self, runtime: &tokio::runtime::Handle, ctx: &egui::Context, work: Work) {
        if self.busy() {
            return;
        }
        let mut client = if matches!(
            work,
            Work::Login(_) | Work::Restore(_) | Work::SavedPassword
        ) {
            match Client::new(self.settings.clone()) {
                Ok(c) => c,
                Err(e) => {
                    self.error = e.to_string();
                    return;
                }
            }
        } else if let Some(client) = self.client.clone() {
            client
        } else {
            self.error = "请先登录 JumpServer".into();
            self.open = true;
            return;
        };
        if matches!(
            work,
            Work::Sync | Work::Accounts(_) | Work::PreferenceAccounts | Work::Connect(..)
        ) && !client.logged_in()
        {
            self.error = "请先登录并完成 MFA".into();
            self.open = true;
            return;
        }
        self.error.clear();
        if matches!(work, Work::Sync) {
            // Account lookups and connections must not postpone asset refresh indefinitely.
            self.next_sync = Instant::now() + self.settings.sync_interval();
        }
        self.status = match work {
            Work::Sync => "正在同步授权资产…",
            Work::Accounts(_) | Work::PreferenceAccounts => "正在查询授权账号…",
            Work::Connect(..) => "正在申请中转连接…",
            _ => "正在认证…",
        }
        .into();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let ctx = ctx.clone();
        let task = runtime.spawn(async move {
            let restoring = matches!(work, Work::Restore(_));
            let result = tokio::time::timeout(Duration::from_secs(180), async {
                Ok::<Payload, anyhow::Error>(match work {
                    Work::Login(password) => Payload::Auth(client.login(&password).await?),
                    Work::SavedPassword => {
                        let id = client.settings.id;
                        let password = tokio::task::spawn_blocking(move || {
                            jms::credentials::read_password(id)
                        })
                        .await??
                        .ok_or_else(|| anyhow::anyhow!("未保存密码，请输入密码并登录"))?;
                        Payload::Auth(client.login(&password).await?)
                    }
                    Work::Restore(token) => {
                        client.restore(token).await?;
                        Payload::Auth(AuthStep::LoggedIn)
                    }
                    Work::Mfa(kind, code) => Payload::Auth(client.verify_mfa(&kind, &code).await?),
                    Work::SendMfa(kind) => {
                        client.send_mfa(&kind).await?;
                        Payload::Sent
                    }
                    Work::Continue => Payload::Auth(client.continue_login().await?),
                    Work::Sync => Payload::Snapshot(client.sync().await?),
                    Work::PreferenceAccounts => {
                        Payload::PreferenceAccounts(client.default_account_candidates().await?)
                    }
                    Work::Accounts(asset) => {
                        let accounts = client.accounts(asset.id).await?;
                        Payload::Accounts(asset, accounts)
                    }
                    Work::Connect(asset, account, username, secret) => Payload::Connected(
                        client.resolve(asset, account, &username, &secret).await?,
                    ),
                })
            })
            .await
            .unwrap_or_else(|_| {
                Err(anyhow::anyhow!(
                    "JumpServer 操作超过 180 秒，已停止；缓存未替换"
                ))
            });
            let _ = tx.send(Reply {
                client,
                restoring,
                result: result.map_err(|e| format!("{e:#}")),
            });
            ctx.request_repaint();
        });
        self.abort = Some(task.abort_handle());
        self.job = Some(rx);
    }
}
impl Drop for State {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl App {
    fn save_jumpserver(&mut self) {
        self.submit(
            Action::SaveJumpServer(self.jumpserver.saved()),
            Pending::Update,
        );
    }

    pub(super) fn restore_jumpserver(
        &mut self,
        ctx: &egui::Context,
        token: Option<Zeroizing<String>>,
    ) {
        if let Some(token) = token {
            self.jumpserver
                .start(self.runtime.handle(), ctx, Work::Restore(token));
        } else {
            self.jumpserver.status = "未保存登录状态，请登录".into();
            self.auto_auth_jumpserver(ctx);
        }
    }

    fn auto_auth_jumpserver(&mut self, ctx: &egui::Context) {
        if self.jumpserver.settings.remember_password
            && !self.jumpserver.auto_auth_blocked
            && !self.jumpserver.busy()
        {
            self.jumpserver.auto_auth_blocked = true;
            self.jumpserver.client = None;
            self.jumpserver
                .start(self.runtime.handle(), ctx, Work::SavedPassword);
        }
    }

    pub(super) fn poll_jumpserver(&mut self, ctx: &egui::Context) {
        if self.jumpserver.client.as_ref().is_some_and(Client::expired) {
            let pending = self
                .jumpserver
                .pending_reconnect
                .take()
                .or_else(|| self.jumpserver.reconnect_inflight.take());
            let queue = std::mem::take(&mut self.jumpserver.reconnect_queue);
            self.jumpserver.cancel();
            self.jumpserver.pending_reconnect = pending;
            self.jumpserver.reconnect_queue = queue;
            self.jumpserver.client = None;
            self.jumpserver.status = "登录保持时间已到，请重新登录并完成 MFA".into();
            self.submit(
                Action::JumpServerToken {
                    id: self.jumpserver.settings.id,
                    token: None,
                },
                Pending::Update,
            );
            self.auto_auth_jumpserver(ctx);
        }
        if self.jumpserver.restore {
            self.jumpserver.restore = false;
            self.submit(
                Action::ReadJumpServerToken(self.jumpserver.settings.id),
                Pending::JumpToken(self.jumpserver.generation),
            );
        }
        let reply = self
            .jumpserver
            .job
            .as_mut()
            .and_then(|r| match r.try_recv() {
                Ok(r) => Some(Ok(r)),
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    Some(Err("后台请求已停止"))
                }
                Err(_) => None,
            });
        if let Some(reply) = reply {
            self.jumpserver.job = None;
            self.jumpserver.abort = None;
            match reply {
                Err(e) => self.jumpserver.error = e.into(),
                Ok(reply) => {
                    self.jumpserver.client = Some(reply.client);
                    match reply.result {
                        Err(error) => {
                            if (self
                                .jumpserver
                                .client
                                .as_ref()
                                .is_some_and(Client::logged_in)
                                && error.contains("HTTP 401"))
                                || (reply.restoring
                                    && (error.contains("过期")
                                        || error.contains("HTTP 401")
                                        || error.contains("不一致")
                                        || error.contains("格式无效")))
                            {
                                self.jumpserver.client = None;
                                self.submit(
                                    Action::JumpServerToken {
                                        id: self.jumpserver.settings.id,
                                        token: None,
                                    },
                                    Pending::Update,
                                );
                            }
                            self.jumpserver.error = error;
                            self.jumpserver.status = "请求失败；保留上次成功列表".into();
                            if self.jumpserver.client.is_none() {
                                if self.jumpserver.pending_reconnect.is_none() {
                                    self.jumpserver.pending_reconnect =
                                        self.jumpserver.reconnect_inflight.take();
                                }
                                self.auto_auth_jumpserver(ctx);
                            }
                            if !self.jumpserver.busy() {
                                if self.jumpserver.mfa.is_empty() && !self.jumpserver.approval {
                                    self.jumpserver.pending_reconnect = None;
                                    self.jumpserver.reconnect_account = None;
                                    self.jumpserver.reconnect_inflight = None;
                                    self.jumpserver.reconnect_queue.clear();
                                }
                                self.jumpserver.open = true;
                            }
                        }
                        Ok(Payload::Auth(AuthStep::Mfa(choices))) => {
                            self.jumpserver.mfa_kind = choices
                                .iter()
                                .find(|s| s.as_str() == "otp")
                                .unwrap_or(&choices[0])
                                .clone();
                            self.jumpserver.mfa = choices;
                            self.jumpserver.code.zeroize();
                            self.jumpserver.mfa_focus = true;
                            self.jumpserver.open = true;
                            self.jumpserver.status = "请输入 MFA 验证码".into();
                        }
                        Ok(Payload::Auth(AuthStep::Approval)) => {
                            self.jumpserver.mfa.clear();
                            self.jumpserver.approval = true;
                            self.jumpserver.open = true;
                            self.jumpserver.status = "等待登录审批，审批后点击检查审批".into();
                        }
                        Ok(Payload::Auth(AuthStep::LoggedIn)) => {
                            self.jumpserver.mfa.clear();
                            self.jumpserver.approval = false;
                            let client = self.jumpserver.client.as_ref().unwrap();
                            if self.jumpserver.snapshot.as_ref().is_some_and(|s| {
                                s.user_id != client.user_id || s.org_id != client.settings.org_id
                            }) {
                                self.jumpserver.snapshot = None;
                            }
                            self.jumpserver.settings.org_id = client.settings.org_id.clone();
                            self.jumpserver.draft.org_id = client.settings.org_id.clone();
                            let token = if self.jumpserver.settings.remember {
                                client.saved_token()
                            } else {
                                None
                            };
                            self.submit(
                                Action::JumpServerToken {
                                    id: self.jumpserver.settings.id,
                                    token,
                                },
                                Pending::Update,
                            );
                            self.save_jumpserver();
                            self.jumpserver.status = "已登录".into();
                            if let Some(reference) = self.jumpserver.pending_reconnect.take() {
                                self.reconnect_jumpserver(ctx, reference);
                            } else if !self.jumpserver.settings.org_id.is_empty() {
                                self.jumpserver
                                    .start(self.runtime.handle(), ctx, Work::Sync);
                            } else {
                                self.jumpserver.open = true;
                                self.jumpserver.status = "请选择组织后同步".into();
                            }
                        }
                        Ok(Payload::Sent) => self.jumpserver.status = "验证码已发送".into(),
                        Ok(Payload::PreferenceAccounts(accounts)) => {
                            self.jumpserver.preference_accounts = accounts;
                            self.jumpserver.status =
                                "已加载授权账号，请选择优先级并保存配置".into();
                        }
                        Ok(Payload::Snapshot(snapshot)) => {
                            self.jumpserver.auto_auth_blocked = false;
                            self.jumpserver.preference_accounts.clear();
                            self.jumpserver.status =
                                format!("已同步 {} 个资产", snapshot.assets.len());
                            self.jumpserver.snapshot = Some(snapshot);
                            self.save_jumpserver();
                        }
                        Ok(Payload::Accounts(asset, accounts)) => {
                            self.jumpserver.status = "请选择授权账号".into();
                            self.jumpserver.input_username.clear();
                            self.jumpserver.input_secret.zeroize();
                            let preferred =
                                if let Some(wanted) = self.jumpserver.reconnect_account.take() {
                                    let matches: Vec<_> = accounts
                                        .iter()
                                        .enumerate()
                                        .filter(|(_, a)| a.username == wanted)
                                        .map(|(i, _)| i)
                                        .collect();
                                    if matches.len() != 1 {
                                        self.jumpserver.error =
                                            "原会话账号已不可用或存在歧义，已停止自动重连".into();
                                        self.jumpserver.reconnect_queue.clear();
                                        self.jumpserver.reconnect_inflight = None;
                                        self.jumpserver.open = true;
                                        return;
                                    }
                                    Some(matches[0])
                                } else {
                                    jms::preferred_account(
                                        &accounts,
                                        &self.jumpserver.settings.default_accounts,
                                    )
                                };
                            if let Some(index) = preferred
                                && accounts[index].has_secret
                                && accounts[index].has_username
                            {
                                self.jumpserver.start(
                                    self.runtime.handle(),
                                    ctx,
                                    Work::Connect(
                                        asset,
                                        accounts[index].clone(),
                                        String::new(),
                                        Zeroizing::new(String::new()),
                                    ),
                                );
                            } else {
                                self.jumpserver.choice = Some((asset, accounts, preferred));
                            }
                        }
                        Ok(Payload::Connected(resolved)) => {
                            self.jumpserver.reconnect_inflight = None;
                            self.jumpserver.auto_auth_blocked = false;
                            let reference = SessionRef {
                                settings: self.jumpserver.settings.clone(),
                                user_id: self.jumpserver.client.as_ref().unwrap().user_id.clone(),
                                asset: resolved.asset.clone(),
                                account: resolved.account.clone(),
                            };
                            let profile = Profile {
                                id: Uuid::new_v4(),
                                name: resolved.asset.name.clone(),
                                host: reference.settings.ssh_host.clone(),
                                port: reference.settings.ssh_port,
                                username: resolved.username,
                                remember_secret: false,
                                ..Default::default()
                            };
                            self.start_connection(ctx, profile, resolved.secret.to_string());
                            let tab = self.tabs.last_mut().unwrap();
                            // Keep the one-time username out of the GUI's profile and all export paths.
                            tab.profile.username = resolved.account;
                            tab.jumpserver = Some(reference);
                            self.jumpserver.recent.retain(|a| a.id != resolved.asset.id);
                            self.jumpserver.recent.insert(0, resolved.asset);
                            self.jumpserver.recent.truncate(20);
                            self.jumpserver.status = "已发起 KoKo SSH 中转连接".into();
                        }
                    }
                }
            }
        }
        if !self.jumpserver.busy()
            && self.jumpserver.mfa.is_empty()
            && !self.jumpserver.approval
            && self.jumpserver.choice.is_none()
            && self.jumpserver.pending_reconnect.is_none()
            && let Some(reference) = self.jumpserver.reconnect_queue.pop_front()
        {
            self.reconnect_jumpserver(ctx, reference);
        }
        if self.jumpserver.settings.auto_sync
            && self.jumpserver.authenticated()
            && !self.jumpserver.busy()
            && !self.jumpserver.settings.org_id.is_empty()
            && Instant::now() >= self.jumpserver.next_sync
        {
            self.jumpserver
                .start(self.runtime.handle(), ctx, Work::Sync);
        }
        if self.jumpserver.authenticated() {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }

    pub(super) fn open_jump_asset(&mut self, ctx: &egui::Context, asset: Asset) {
        self.jumpserver
            .start(self.runtime.handle(), ctx, Work::Accounts(asset));
    }

    pub(super) fn queue_jumpserver_reconnect(&mut self, reference: SessionRef) {
        if self.jumpserver.settings.remember_password
            && (!self.jumpserver.auto_auth_blocked
                || self.jumpserver.busy()
                || !self.jumpserver.mfa.is_empty())
            && self.jumpserver.settings.same_identity(&reference.settings)
            && self.jumpserver.reconnect_queue.len() < 20
        {
            self.jumpserver.reconnect_queue.push_back(reference);
        }
    }

    pub(super) fn reconnect_jumpserver(&mut self, ctx: &egui::Context, reference: SessionRef) {
        if !self.jumpserver.settings.same_identity(&reference.settings) {
            self.message = "请登录此会话原来的 JumpServer 实例、用户和组织后再重连".into();
            self.jumpserver.open = true;
            return;
        }
        if !self.jumpserver.authenticated() {
            self.jumpserver.pending_reconnect = Some(reference);
            if !self.jumpserver.busy()
                && self.jumpserver.mfa.is_empty()
                && !self.jumpserver.approval
            {
                self.jumpserver.auto_auth_blocked = false;
                self.auto_auth_jumpserver(ctx);
            }
            if !self.jumpserver.busy() {
                self.jumpserver.open = true;
            }
            return;
        }
        if !self
            .jumpserver
            .client
            .as_ref()
            .is_some_and(|c| c.user_id == reference.user_id)
        {
            self.message = "登录用户与原会话不一致，已取消重连".into();
            return;
        }
        if self.jumpserver.busy() {
            if self.jumpserver.reconnect_queue.len() < 20 {
                self.jumpserver.reconnect_queue.push_back(reference);
            }
            return;
        }
        self.jumpserver.reconnect_inflight = Some(reference.clone());
        self.jumpserver.reconnect_account = Some(reference.account);
        self.open_jump_asset(ctx, reference.asset);
    }

    pub(super) fn jumpserver_tree(&mut self, ui: &mut egui::Ui) {
        if self.config.jumpserver.is_none() && self.jumpserver.snapshot.is_none() {
            return;
        }
        let mut connect = None;
        let mut refresh = false;
        egui::CollapsingHeader::new("JumpServer")
            .id_salt("jumpserver_root")
            .default_open(true)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.strong(&self.jumpserver.settings.name);
                    if ui.small_button("配置").clicked() {
                        self.jumpserver.open = true;
                    }
                    if ui
                        .add_enabled(
                            self.jumpserver.authenticated() && !self.jumpserver.busy(),
                            egui::Button::new("刷新").small(),
                        )
                        .clicked()
                    {
                        refresh = true;
                    }
                });
                ui.small(&self.jumpserver.status);
                if !self.jumpserver.error.is_empty() {
                    ui.colored_label(Color32::LIGHT_RED, &self.jumpserver.error);
                }
                if let Some(snapshot) = &self.jumpserver.snapshot {
                    let filter = self.filter.to_lowercase();
                    let org = self
                        .jumpserver
                        .client
                        .as_ref()
                        .and_then(|c| c.organizations.iter().find(|o| o.id == snapshot.org_id))
                        .map(|o| o.name.as_str())
                        .unwrap_or(&snapshot.org_id);
                    egui::CollapsingHeader::new(org)
                        .default_open(true)
                        .show(ui, |ui| {
                            if !filter.is_empty() {
                                for asset in &snapshot.assets {
                                    if format!("{} {}", asset.name, asset.address)
                                        .to_lowercase()
                                        .contains(&filter)
                                    {
                                        asset_row(
                                            ui,
                                            asset,
                                            self.jumpserver.authenticated(),
                                            &mut connect,
                                        );
                                    }
                                }
                            } else {
                                render_nodes(
                                    ui,
                                    snapshot,
                                    None,
                                    self.jumpserver.authenticated(),
                                    &mut connect,
                                    0,
                                );
                                let nodes: std::collections::HashSet<_> =
                                    snapshot.nodes.iter().map(|n| n.id).collect();
                                let ungrouped: Vec<_> = snapshot
                                    .assets
                                    .iter()
                                    .filter(|a| !a.nodes.iter().any(|n| nodes.contains(n)))
                                    .collect();
                                if !ungrouped.is_empty() {
                                    egui::CollapsingHeader::new("未分组资产").show(ui, |ui| {
                                        for asset in ungrouped {
                                            asset_row(
                                                ui,
                                                asset,
                                                self.jumpserver.authenticated(),
                                                &mut connect,
                                            );
                                        }
                                    });
                                }
                            }
                        });
                }
            });
        if refresh {
            self.jumpserver
                .start(self.runtime.handle(), ui.ctx(), Work::Sync);
        }
        if let Some(asset) = connect {
            self.open_jump_asset(ui.ctx(), asset);
        }
    }

    pub(super) fn jumpserver_dialog(&mut self, ctx: &egui::Context) {
        let mut action = 0;
        if self.jumpserver.open {
            let mut open = true;
            egui::Window::new("JumpServer 堡垒机配置")
                .id(egui::Id::unique("jumpserver_settings"))
                .open(&mut open).default_width(580.0)
                .default_pos(ctx.content_rect().center() - egui::vec2(290.0, 330.0))
                .resizable(true).show(ctx, |ui| {
                    let state = &mut self.jumpserver;
                    let busy = state.busy();
                    ui.horizontal(|ui| {
                        ui.strong("JumpServer");
                        ui.weak("v4.10.2 社区版 · SSH 中转");
                    });
                    ui.separator();
                    egui::ScrollArea::vertical().id_salt("jumpserver_form")
                        .max_height((ctx.content_rect().height() - 260.0).max(180.0))
                        .show(ui, |ui| {
                        ui.add_enabled_ui(!busy, |ui| {
                            let field_width = (ui.available_width() - 120.0).clamp(160.0, 430.0);
                            ui.strong("连接信息");
                            egui::Grid::new("jumpserver_fields").min_col_width(90.0).num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                                for (label, text) in [("实例名称", &mut state.draft.name), ("Web / API 地址", &mut state.draft.url)] {
                                    ui.label(label); ui.add(egui::TextEdit::singleline(text).desired_width(field_width)); ui.end_row();
                                }
                                ui.label("KoKo SSH 主机");
                                ui.horizontal(|ui| {
                                    ui.add(egui::TextEdit::singleline(&mut state.draft.ssh_host).desired_width((field_width - 110.0).max(100.0)));
                                    ui.label("端口");
                                    ui.add(egui::DragValue::new(&mut state.draft.ssh_port).range(1..=65535));
                                }); ui.end_row();
                            });
                            ui.checkbox(&mut state.draft.allow_http, "允许 HTTP（密码、验证码和令牌将明文传输）");
                            ui.add_space(8.0); ui.separator(); ui.strong("登录设置");
                            egui::Grid::new("jumpserver_login").min_col_width(90.0).num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                                ui.label("用户名"); ui.add(egui::TextEdit::singleline(&mut state.draft.username).desired_width(field_width)); ui.end_row();
                                ui.label("密码"); ui.add(egui::TextEdit::singleline(&mut *state.password).password(true).desired_width(field_width)); ui.end_row();
                            });
                            ui.horizontal_wrapped(|ui| {
                                ui.checkbox(&mut state.draft.remember, "记住登录状态").on_hover_text("使用 Windows 凭据管理器保存登录令牌，不保存密码");
                                ui.label("登录保持时间");
                                ui.add(egui::DragValue::new(&mut state.draft.keep_minutes).range(1..=43200).suffix(" 分钟"));
                            });
                            ui.checkbox(&mut state.draft.remember_password, "记住密码，到期自动登录 / 异常断线重连")
                                .on_hover_text("密码仅保存到 Windows 凭据管理器；需要 MFA 时弹出验证码窗口。取消勾选并保存配置会删除密码。");
                            ui.small("有效期以本地与服务端较短值为准；延长保持时间需重新登录。");
                            ui.add_space(8.0); ui.separator(); ui.strong("资产同步");
                            ui.horizontal_wrapped(|ui| {
                                ui.checkbox(&mut state.draft.auto_sync, "自动刷新授权资产");
                                ui.add_enabled(state.draft.auto_sync, egui::DragValue::new(&mut state.draft.sync_interval_minutes).range(1..=43200).suffix(" 分钟"));
                            });
                            if let Some(client) = &state.client && client.logged_in() {
                                ui.label(format!("已登录：{} · 剩余 {} 分钟", client.username, client.remaining_seconds().div_ceil(60)));
                                let before = state.draft.org_id.clone();
                                egui::ComboBox::from_label("组织").selected_text(client.organizations.iter().find(|o| o.id == state.draft.org_id).map(|o| o.name.as_str()).unwrap_or("请选择组织"))
                                    .show_ui(ui, |ui| { for org in &client.organizations { ui.selectable_value(&mut state.draft.org_id, org.id.clone(), &org.name); } });
                                if before != state.draft.org_id { action = 5; }
                            }
                            ui.add_space(8.0); ui.separator();
                            egui::CollapsingHeader::new("默认连接账号（可选）").id_salt("jumpserver_account_preferences").show(ui, |ui| {
                                if !state.settings.same_identity(&state.draft) { state.preference_accounts.clear(); }
                                if ui.add_enabled(!busy && state.authenticated() && state.settings.same_identity(&state.draft), egui::Button::new("加载 / 刷新授权账号")).clicked() { action = 10; }
                                ui.small("从当前组织的授权账号中选择全局默认值，无需选择资产。选择后保存配置。");
                                ui.small(format!("已加载 {} 个候选；连接时按目标资产最新授权匹配。", state.preference_accounts.len()));
                                egui::Grid::new("jumpserver_default_accounts").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                                    for (i, account) in state.draft.default_accounts.iter_mut().enumerate() {
                                        ui.label(["一级默认账号", "二级默认账号", "三级默认账号"][i]);
                                        egui::ComboBox::from_id_salt(("jumpserver_preference", i))
                                            .selected_text(if account.is_empty() { "不指定" } else { account.as_str() })
                                            .show_ui(ui, |ui| {
                                                ui.selectable_value(account, String::new(), "不指定");
                                                for candidate in &state.preference_accounts {
                                                    ui.selectable_value(account, candidate.clone(), candidate);
                                                }
                                            });
                                        ui.end_row();
                                    }
                                });
                                ui.small("按一级 → 二级 → 三级匹配授权账号；无匹配或有歧义时手动选择。");
                            });
                            if state.approval && ui.button("检查审批").clicked() { action = 8; }
                        });
                    });
                    ui.separator();
                    ui.horizontal_wrapped(|ui| { if busy { ui.spinner(); } ui.label(&state.status); });
                    if let Some(snapshot) = &state.snapshot {
                        let elapsed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs().saturating_sub(snapshot.timestamp);
                        ui.small(format!("上次成功同步：{elapsed} 秒前 · {} 个资产", snapshot.assets.len()));
                    }
                    if !state.error.is_empty() { ui.colored_label(Color32::LIGHT_RED, &state.error); }
                    ui.separator();
                    ui.horizontal_wrapped(|ui| {
                        if ui.add_enabled(!busy, egui::Button::new("登录并同步").fill(ui.visuals().selection.bg_fill)).clicked() { action = 2; }
                        if ui.add_enabled(!busy, egui::Button::new("保存配置")).clicked() { action = 1; }
                        if ui.add_enabled(!busy && state.authenticated(), egui::Button::new("立即同步")).clicked() { action = 3; }
                        if busy && ui.button("取消请求").clicked() { state.cancel(); state.status = "请求已取消".into(); }
                        if ui.button("关闭").clicked() { state.open = false; }
                    });
                    if ui.small_button("退出登录 / 清除已保存登录").on_hover_text("已有终端保持连接，可通过终端的断开操作结束会话").clicked() { action = 4; }
                });
            self.jumpserver.open &= open;
        }
        if !self.jumpserver.mfa.is_empty() {
            egui::Modal::new(egui::Id::unique("jumpserver_mfa")).show(ctx, |ui| {
                let state = &mut self.jumpserver;
                ui.set_max_width(420.0);
                ui.heading("JumpServer MFA 验证");
                ui.label(format!(
                    "{} · {}",
                    state.settings.name, state.settings.username
                ));
                ui.label("请输入验证码以继续登录并同步资产。");
                let busy = state.busy();
                ui.add_enabled_ui(!busy, |ui| {
                    let previous = state.mfa_kind.clone();
                    egui::ComboBox::from_label("认证方式")
                        .selected_text(&state.mfa_kind)
                        .show_ui(ui, |ui| {
                            for kind in &state.mfa {
                                ui.selectable_value(&mut state.mfa_kind, kind.clone(), kind);
                            }
                        });
                    if previous != state.mfa_kind {
                        state.code.zeroize();
                        state.mfa_focus = true;
                    }
                    let input = ui.add(
                        egui::TextEdit::singleline(&mut *state.code)
                            .id(egui::Id::unique("jumpserver_mfa_code"))
                            .password(true)
                            .hint_text("验证码")
                            .desired_width(280.0),
                    );
                    if state.mfa_focus {
                        input.request_focus();
                        state.mfa_focus = false;
                    }
                    let enter = input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui
                        .add_enabled(
                            !state.code.trim().is_empty(),
                            egui::Button::new("验证并继续"),
                        )
                        .clicked()
                        || (enter && !state.code.trim().is_empty())
                    {
                        action = 6;
                    }
                    if matches!(state.mfa_kind.as_str(), "sms" | "email")
                        && ui.button("发送验证码").clicked()
                    {
                        action = 7;
                    }
                });
                if busy {
                    ui.spinner();
                }
                ui.label(&state.status);
                if !state.error.is_empty() {
                    ui.colored_label(Color32::LIGHT_RED, &state.error);
                }
                if ui.button("取消验证").clicked() {
                    action = 9;
                }
            });
        }
        match action {
            1 | 2 => match self.jumpserver.draft.validate() {
                Err(e) => self.jumpserver.error = e.to_string(),
                Ok(_) => {
                    let password = std::mem::replace(
                        &mut self.jumpserver.password,
                        Zeroizing::new(String::new()),
                    );
                    let old_id = self.jumpserver.settings.id;
                    if !self
                        .jumpserver
                        .settings
                        .same_identity(&self.jumpserver.draft)
                    {
                        self.jumpserver.logout();
                        self.jumpserver.draft.id = Uuid::new_v4();
                        self.submit(
                            Action::JumpServerToken {
                                id: old_id,
                                token: None,
                            },
                            Pending::Update,
                        );
                    }
                    if old_id != self.jumpserver.draft.id {
                        self.submit(
                            Action::JumpServerPassword {
                                id: old_id,
                                password: None,
                            },
                            Pending::Update,
                        );
                    }
                    self.jumpserver.settings = self.jumpserver.draft.clone();
                    if !self.jumpserver.settings.remember_password || !password.is_empty() {
                        let saved_password = self
                            .jumpserver
                            .settings
                            .remember_password
                            .then(|| password.clone());
                        self.submit(
                            Action::JumpServerPassword {
                                id: self.jumpserver.settings.id,
                                password: saved_password,
                            },
                            Pending::Update,
                        );
                    }
                    self.jumpserver.next_sync =
                        Instant::now() + self.jumpserver.settings.sync_interval();
                    if let Some(c) = &mut self.jumpserver.client {
                        c.settings = self.jumpserver.settings.clone();
                        c.shorten_session();
                    }
                    self.save_jumpserver();
                    let token = if action == 1 && self.jumpserver.settings.remember {
                        self.jumpserver
                            .client
                            .as_ref()
                            .and_then(Client::saved_token)
                    } else {
                        None
                    };
                    self.submit(
                        Action::JumpServerToken {
                            id: self.jumpserver.settings.id,
                            token,
                        },
                        Pending::Update,
                    );
                    if action == 2 {
                        let pending = self.jumpserver.pending_reconnect.take();
                        self.jumpserver.cancel();
                        self.jumpserver.pending_reconnect = pending;
                        self.jumpserver.auto_auth_blocked = true;
                        self.jumpserver.client = None;
                        self.jumpserver.mfa.clear();
                        self.jumpserver.approval = false;
                        self.jumpserver.start(
                            self.runtime.handle(),
                            ctx,
                            if password.is_empty() && self.jumpserver.settings.remember_password {
                                Work::SavedPassword
                            } else {
                                Work::Login(password)
                            },
                        );
                    } else {
                        self.jumpserver.status = "配置保存已提交".into();
                    }
                }
            },
            3 => {
                if self.jumpserver.draft != self.jumpserver.settings {
                    self.jumpserver.error = "配置有未保存修改，请先保存配置".into();
                } else {
                    self.jumpserver
                        .start(self.runtime.handle(), ctx, Work::Sync);
                }
            }
            4 => {
                self.jumpserver.logout();
                self.jumpserver.settings.remember_password = false;
                self.jumpserver.draft.remember_password = false;
                self.submit(
                    Action::JumpServerPassword {
                        id: self.jumpserver.settings.id,
                        password: None,
                    },
                    Pending::Update,
                );
                if self.config.jumpserver.is_some() {
                    self.save_jumpserver();
                }
                self.submit(
                    Action::JumpServerToken {
                        id: self.jumpserver.settings.id,
                        token: None,
                    },
                    Pending::Update,
                );
            }
            5 => {
                self.jumpserver.cancel();
                self.jumpserver.snapshot = None;
                self.jumpserver.recent.clear();
                self.jumpserver.settings.org_id = self.jumpserver.draft.org_id.clone();
                if let Some(c) = &mut self.jumpserver.client {
                    c.settings.org_id = self.jumpserver.settings.org_id.clone();
                }
                self.save_jumpserver();
                self.jumpserver
                    .start(self.runtime.handle(), ctx, Work::Sync);
            }
            6 => {
                let code =
                    std::mem::replace(&mut self.jumpserver.code, Zeroizing::new(String::new()));
                self.jumpserver.start(
                    self.runtime.handle(),
                    ctx,
                    Work::Mfa(self.jumpserver.mfa_kind.clone(), code),
                );
            }
            7 => self.jumpserver.start(
                self.runtime.handle(),
                ctx,
                Work::SendMfa(self.jumpserver.mfa_kind.clone()),
            ),
            8 => self
                .jumpserver
                .start(self.runtime.handle(), ctx, Work::Continue),
            9 => {
                self.jumpserver.cancel();
                self.jumpserver.client = None;
                self.jumpserver.approval = false;
                self.jumpserver.auto_auth_blocked = true;
                self.jumpserver.status = "已取消 MFA 验证，请重新登录".into();
            }
            10 => {
                self.jumpserver.preference_accounts.clear();
                self.jumpserver
                    .start(self.runtime.handle(), ctx, Work::PreferenceAccounts);
            }
            _ => {}
        }
        let mut connect = false;
        let mut cancel = false;
        if let Some((asset, accounts, selected)) = &mut self.jumpserver.choice {
            egui::Window::new("选择 JumpServer 授权账号")
                .collapsible(false)
                .vscroll(true)
                .max_height((ctx.content_rect().height() - 80.0).max(240.0))
                .show(ctx, |ui| {
                    ui.label(format!("{} · {}", asset.name, asset.address));
                    for (i, account) in accounts.iter().enumerate() {
                        if ui
                            .radio_value(
                                selected,
                                Some(i),
                                format!("{} ({})", account.name, account.username),
                            )
                            .changed()
                        {
                            self.jumpserver.input_secret.zeroize();
                            self.jumpserver.input_username.clear();
                        }
                    }
                    if let Some(index) = *selected {
                        let account = &accounts[index];
                        if !account.has_username {
                            ui.label("本次连接用户名");
                            ui.text_edit_singleline(&mut self.jumpserver.input_username);
                        }
                        if !account.has_secret {
                            ui.label("本次连接密码（不保存）");
                            ui.add(
                                egui::TextEdit::singleline(&mut *self.jumpserver.input_secret)
                                    .password(true),
                            );
                        }
                    }
                    ui.horizontal(|ui| {
                        connect = ui
                            .add_enabled(
                                selected.is_some(),
                                egui::Button::new("通过 JumpServer 连接"),
                            )
                            .clicked();
                        cancel = ui.button("取消").clicked();
                    });
                });
        }
        if connect && let Some((asset, accounts, Some(selected))) = self.jumpserver.choice.take() {
            let secret = std::mem::replace(
                &mut self.jumpserver.input_secret,
                Zeroizing::new(String::new()),
            );
            self.jumpserver.start(
                self.runtime.handle(),
                ctx,
                Work::Connect(
                    asset,
                    accounts[selected].clone(),
                    self.jumpserver.input_username.clone(),
                    secret,
                ),
            );
        }
        if cancel {
            self.jumpserver.choice = None;
            self.jumpserver.input_secret.zeroize();
        }
        if !self.jumpserver.open {
            self.jumpserver.password.zeroize();
            if self.jumpserver.mfa.is_empty() {
                self.jumpserver.code.zeroize();
            }
        }
    }
}

fn asset_row(ui: &mut egui::Ui, asset: &Asset, authenticated: bool, connect: &mut Option<Asset>) {
    let response = ui
        .add_enabled(
            authenticated && asset.is_active,
            egui::Button::new(&asset.name).frame(false),
        )
        .on_hover_text(format!(
            "{}\n资产 ID：{}\n通过 JumpServer 连接；连接时检查 SSH 授权",
            asset.address, asset.id
        ));
    if response.double_clicked() {
        *connect = Some(asset.clone());
    }
    response.context_menu(|ui| {
        if ui.button("通过 JumpServer 连接").clicked() {
            *connect = Some(asset.clone());
            ui.close();
        }
    });
}

fn render_nodes(
    ui: &mut egui::Ui,
    snapshot: &Snapshot,
    parent: Option<&str>,
    authenticated: bool,
    connect: &mut Option<Asset>,
    depth: usize,
) {
    if depth >= 64 {
        return;
    }
    for node in &snapshot.nodes {
        let parent_key = node.key.rsplit_once(':').map(|(p, _)| p);
        let visible_parent = parent_key.filter(|p| snapshot.nodes.iter().any(|n| n.key == *p));
        if visible_parent != parent {
            continue;
        }
        egui::CollapsingHeader::new(&node.name)
            .id_salt(("jump_node", node.id))
            .show(ui, |ui| {
                render_nodes(
                    ui,
                    snapshot,
                    Some(&node.key),
                    authenticated,
                    connect,
                    depth + 1,
                );
                for asset in snapshot
                    .assets
                    .iter()
                    .filter(|a| a.nodes.contains(&node.id))
                {
                    asset_row(ui, asset, authenticated, connect);
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> (tempfile::TempDir, egui::Context, App) {
        let root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/jumpserver-app-tests");
        std::fs::create_dir_all(&root).unwrap();
        let temp = tempfile::tempdir_in(root).unwrap();
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let app = App::new(
            &cc,
            Config::default(),
            temp.path().join("config.json"),
            runtime,
            true,
            false,
        )
        .unwrap();
        (temp, ctx, app)
    }

    fn frame(app: &mut App, ctx: &egui::Context) -> egui::FullOutput {
        frame_events(app, ctx, vec![])
    }

    fn frame_events(
        app: &mut App,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1100.0, 900.0),
                )),
                events,
                focused: true,
                ..Default::default()
            },
            |ui| {
                app.jumpserver_dialog(ctx);
                egui::CentralPanel::default().show(ui, |_| {});
            },
        );
        output.textures_delta.clear();
        output
    }

    fn test_settings() -> Settings {
        Settings {
            url: "http://127.0.0.1:9".into(),
            ssh_host: "127.0.0.1".into(),
            username: "synthetic-user".into(),
            allow_http: true,
            ..Default::default()
        }
    }

    fn reference(settings: &Settings) -> SessionRef {
        SessionRef { settings: settings.clone(), user_id: "synthetic-id".into(),
            asset: serde_json::from_value(serde_json::json!({"id":Uuid::new_v4(), "name":"test", "address":"127.0.0.1", "protocols":[]})).unwrap(),
            account: "original-account".into() }
    }

    #[test]
    fn reconnect_without_password_waits_and_logout_cancels_it() {
        let (_temp, ctx, mut app) = app();
        app.jumpserver.settings = test_settings();
        let reference = reference(&app.jumpserver.settings);
        app.reconnect_jumpserver(&ctx, reference);
        assert!(app.jumpserver.pending_reconnect.is_some());
        assert!(!app.jumpserver.busy());
        app.jumpserver.logout();
        assert!(app.jumpserver.pending_reconnect.is_none());
        assert!(app.jumpserver.auto_auth_blocked);
        app.auto_auth_jumpserver(&ctx);
        assert!(!app.jumpserver.busy());
    }

    #[test]
    fn reconnect_never_falls_back_to_another_single_account() {
        let (_temp, ctx, mut app) = app();
        app.jumpserver.settings = test_settings();
        app.jumpserver.reconnect_account = Some("original-account".into());
        let (tx, rx) = tokio::sync::oneshot::channel();
        app.jumpserver.job = Some(rx);
        let account: Account = serde_json::from_value(serde_json::json!({
            "alias":"other", "name":"other", "username":"other", "has_secret":true,
            "has_username":true, "actions":["connect"]
        }))
        .unwrap();
        assert!(
            tx.send(Reply {
                client: Client::new(app.jumpserver.settings.clone()).unwrap(),
                restoring: false,
                result: Ok(Payload::Accounts(
                    reference(&app.jumpserver.settings).asset,
                    vec![account]
                ))
            })
            .is_ok()
        );
        app.poll_jumpserver(&ctx);
        assert!(app.jumpserver.error.contains("原会话账号"));
        assert!(!app.jumpserver.busy());
        assert!(app.tabs.iter().all(|t| t.jumpserver.is_none()));
    }

    #[test]
    #[ignore = "explicit Windows Credential Manager and loopback authentication validation"]
    fn saved_password_auto_auth_mfa_and_failure_stop() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline);
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(e) => panic!("loopback accept: {e}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            loop {
                let mut buf = [0; 1024];
                let n = stream.read(&mut buf).unwrap();
                assert!(n > 0 && request.len() < 16384);
                request.extend_from_slice(&buf[..n]);
                if let Some(end) = request.windows(4).position(|s| s == b"\r\n\r\n") {
                    let headers = std::str::from_utf8(&request[..end]).unwrap();
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            let (key, value) = line.split_once(':')?;
                            key.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    if request.len() >= end + 4 + length {
                        let body: serde_json::Value =
                            serde_json::from_slice(&request[end + 4..end + 4 + length]).unwrap();
                        assert!(body["password"] == "synthetic-password");
                        break;
                    }
                }
            }
            let body = r#"{"error":"mfa_required","data":{"choices":["otp"]}}"#;
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        });
        let (_temp, ctx, mut app) = app();
        let settings = Settings {
            url: format!("http://{address}"),
            remember_password: true,
            ..test_settings()
        };
        struct Cleanup(Uuid);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = jms::credentials::delete_password(self.0);
            }
        }
        let _cleanup = Cleanup(settings.id);
        jms::credentials::write_password(settings.id, "synthetic-password").unwrap();
        app.jumpserver.settings = settings.clone();
        app.jumpserver.draft = settings.clone();
        app.reconnect_jumpserver(&ctx, reference(&settings));
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.jumpserver.busy() {
            assert!(Instant::now() < deadline);
            app.poll_jumpserver(&ctx);
            std::thread::sleep(Duration::from_millis(10));
        }
        server.join().unwrap();
        assert_eq!(app.jumpserver.mfa, vec!["otp"]);
        assert!(app.jumpserver.pending_reconnect.is_some());
        assert!(app.jumpserver.password.is_empty());
        // Wrong MFA preserves the requested reconnect so another code can finish it.
        let (tx, rx) = tokio::sync::oneshot::channel();
        app.jumpserver.job = Some(rx);
        assert!(
            tx.send(Reply {
                client: app.jumpserver.client.clone().unwrap(),
                restoring: false,
                result: Err("HTTP 401 验证码错误".into())
            })
            .is_ok()
        );
        app.poll_jumpserver(&ctx);
        assert!(app.jumpserver.pending_reconnect.is_some());
        app.jumpserver.cancel();
        app.jumpserver.client = None;
        app.auto_auth_jumpserver(&ctx);
        assert!(!app.jumpserver.busy());
        assert!(app.jumpserver.pending_reconnect.is_none());
    }

    #[test]
    fn options_action_opens_configuration_with_retention_and_three_priorities() {
        let (_temp, ctx, mut app) = app();
        app.chrome_action(15, &ctx);
        assert!(app.jumpserver.modal_open());
        for _ in 0..4 {
            frame(&mut app, &ctx);
        }
        let output = frame(&mut app, &ctx);
        let text: String = output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some(t.galley.job.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        for label in [
            "JumpServer 堡垒机配置",
            "登录保持时间",
            "记住密码，到期自动登录 / 异常断线重连",
            "默认连接账号（可选）",
            "连接信息",
            "资产同步",
            "登录并同步",
        ] {
            assert!(
                text.contains(label),
                "missing visible configuration field: {label}"
            );
        }
    }

    #[test]
    fn default_account_panel_shows_three_global_selectors_without_asset_picker() {
        let (_temp, ctx, mut app) = app();
        app.jumpserver.open = true;
        app.jumpserver.draft.default_accounts = ["deploy".into(), "ops".into(), "legacy".into()];
        for _ in 0..4 {
            frame(&mut app, &ctx);
        }
        let output = frame(&mut app, &ctx);
        let pos = output
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.job.text == "默认连接账号（可选）" => {
                    Some(t.pos + egui::vec2(10.0, 5.0))
                }
                _ => None,
            })
            .expect("preferences header visible");
        for pressed in [true, false] {
            frame_events(
                &mut app,
                &ctx,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    },
                ],
            );
        }
        ctx.style_mut_of(egui::Theme::Dark, |s| s.animation_time = 0.0);
        ctx.style_mut_of(egui::Theme::Light, |s| s.animation_time = 0.0);
        frame(&mut app, &ctx);
        let output = frame(&mut app, &ctx);
        let text = output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some(t.galley.job.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        for label in [
            "加载 / 刷新授权账号",
            "一级默认账号",
            "二级默认账号",
            "三级默认账号",
            "legacy",
        ] {
            assert!(text.contains(label), "missing {label}: {text}");
        }
        assert!(!text.contains("选择账号来源资产"));
        app.jumpserver.preference_accounts = vec!["deploy".into()];
        app.jumpserver.cancel();
        assert!(app.jumpserver.preference_accounts.is_empty());
        assert_eq!(app.jumpserver.draft.default_accounts[2], "legacy");
    }

    #[test]
    fn mfa_challenge_opens_focused_modal_and_cancel_clears_code() {
        let (_temp, ctx, mut app) = app();
        let settings = Settings {
            url: "https://example.invalid".into(),
            ssh_host: "example.invalid".into(),
            username: "alice".into(),
            ..Default::default()
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        app.jumpserver.job = Some(rx);
        assert!(
            tx.send(Reply {
                client: Client::new(settings).unwrap(),
                restoring: false,
                result: Ok(Payload::Auth(AuthStep::Mfa(vec!["otp".into()]))),
            })
            .is_ok()
        );
        app.poll_jumpserver(&ctx);
        app.jumpserver.open = false;
        assert!(app.jumpserver.modal_open());
        frame(&mut app, &ctx);
        let output = frame(&mut app, &ctx);
        assert!(output.shapes.iter().any(|s| matches!(&s.shape,
            egui::Shape::Text(t) if t.galley.job.text.contains("JumpServer MFA 验证"))));
        assert!(ctx.memory(|m| m.has_focus(egui::Id::unique("jumpserver_mfa_code"))));
        app.jumpserver.code.push_str("123456");
        frame(&mut app, &ctx);
        assert_eq!(app.jumpserver.code.as_str(), "123456");
        app.jumpserver.cancel();
        assert!(app.jumpserver.code.is_empty());
        assert!(!app.jumpserver.modal_open());
    }

    #[test]
    fn unrelated_reply_does_not_postpone_asset_refresh() {
        let (_temp, ctx, mut app) = app();
        let settings = Settings {
            url: "https://example.invalid".into(),
            ssh_host: "example.invalid".into(),
            username: "alice".into(),
            ..Default::default()
        };
        let deadline = Instant::now() - Duration::from_secs(1);
        app.jumpserver.next_sync = deadline;
        let (tx, rx) = tokio::sync::oneshot::channel();
        app.jumpserver.job = Some(rx);
        assert!(
            tx.send(Reply {
                client: Client::new(settings).unwrap(),
                restoring: false,
                result: Ok(Payload::Sent),
            })
            .is_ok()
        );
        app.poll_jumpserver(&ctx);
        assert_eq!(app.jumpserver.next_sync, deadline);
        assert!(!app.jumpserver.busy());
    }

    #[test]
    fn logout_invalidates_late_results_and_clears_identity_cache() {
        let mut state = State::new(None);
        let generation = state.generation;
        let (tx, rx) = tokio::sync::oneshot::channel();
        state.job = Some(rx);
        state.snapshot = Some(Snapshot::default());
        state.password = Zeroizing::new("synthetic-password".into());
        state.code = Zeroizing::new("123456".into());
        state.logout();
        assert_ne!(state.generation, generation);
        assert!(tx.is_closed());
        assert!(state.snapshot.is_none() && state.client.is_none() && state.job.is_none());
        assert!(state.password.is_empty() && state.code.is_empty());
    }

    #[test]
    fn jumpserver_reconnect_without_original_login_never_opens_direct_editor() {
        let (_temp, ctx, mut app) = app();
        let reference = SessionRef {
            settings: Settings::default(),
            user_id: Uuid::new_v4().to_string(),
            account: "deploy".into(),
            asset: Asset {
                id: Uuid::new_v4(),
                name: "test-asset".into(),
                address: "192.0.2.10".into(),
                nodes: vec![],
                is_active: true,
            },
        };
        let mut tab = App::make_tab(Profile {
            host: "koko.invalid".into(),
            username: "deploy".into(),
            ..Default::default()
        });
        tab.jumpserver = Some(reference);
        app.tabs = vec![tab];
        app.active = 0;
        app.chrome_action(2, &ctx);
        assert!(app.editor.is_none() && app.jumpserver.open && app.tabs.len() == 1);
        app.tab_action(0, TabAction::Duplicate, &ctx);
        app.tab_action(0, TabAction::Channel, &ctx);
        assert!(app.editor.is_none() && app.tabs.len() == 1);
        assert!(app.tabs[0].endpoint_label().contains("192.0.2.10"));
    }
}
