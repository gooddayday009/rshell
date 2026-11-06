use crate::composer_icons::{self, Icon};
use eframe::egui;
use rshell_core::{
    connection::Connection,
    transfer::{self, Entry, Job, Outcome, Request, Resume},
};
use std::path::PathBuf;
use tokio::sync::oneshot;

const MAX_TASKS: usize = 500;
#[derive(Clone)]
struct DragEntry {
    remote: bool,
    path: String,
    name: String,
}
#[derive(PartialEq)]
enum State {
    Waiting,
    Running,
    Pausing,
    Paused,
    Failed,
    Done,
}
struct Task {
    request: Request,
    resume: Resume,
    name: String,
    state: State,
    job: Option<Job>,
    bytes: u64,
    detail: String,
}
type LocalResult = Result<(PathBuf, Vec<Entry>), String>;
struct FileAction {
    remote: bool,
    path: String,
    new_name: Option<String>,
}
pub struct Browser {
    action: Option<FileAction>,
    mutation: Option<(bool, oneshot::Receiver<Result<(), String>>)>,
    pub open: bool,
    path: String,
    remote_dir: String,
    entries: Vec<Entry>,
    local_path: String,
    local_dir: PathBuf,
    local_entries: Vec<Entry>,
    status: String,
    local_status: String,
    listing: Option<Job>,
    local_listing: Option<oneshot::Receiver<LocalResult>>,
    tasks: Vec<Task>,
    remote_selected: Option<usize>,
    local_selected: Option<usize>,
    initialized: bool,
    column_split: f32,
    queue_split: f32,
    task_filter: usize,
    name_filters: [String; 2],
}
impl Default for Browser {
    fn default() -> Self {
        let home = std::env::var_os("USERPROFILE")
            .map(|p| PathBuf::from(p).join("Desktop"))
            .unwrap_or_else(|| PathBuf::from("C:\\"));
        Self {
            action: None,
            mutation: None,
            open: false,
            path: ".".into(),
            remote_dir: ".".into(),
            entries: vec![],
            local_path: home.to_string_lossy().into_owned(),
            local_dir: home,
            local_entries: vec![],
            status: String::new(),
            local_status: String::new(),
            listing: None,
            local_listing: None,
            tasks: vec![],
            remote_selected: None,
            local_selected: None,
            initialized: false,
            column_split: 0.5,
            queue_split: 0.55,
            task_filter: 0,
            name_filters: Default::default(),
        }
    }
}
fn visible_entries(entries: &[Entry], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| e.name.to_lowercase().contains(&query).then_some(i))
        .collect()
}
fn file_row(ui: &mut egui::Ui, entry: &Entry, selected: bool, index: usize) -> egui::Response {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 25.0), egui::Sense::hover());
    let response = ui
        .interact(
            rect,
            ui.scope_id().with(("file-row", index)),
            egui::Sense::click_and_drag(),
        )
        .on_hover_text(&entry.name);
    let active = selected || response.clicked();
    if active || response.hovered() {
        ui.painter().rect_filled(
            rect,
            3.0,
            if active {
                ui.visuals().selection.bg_fill
            } else {
                ui.visuals().widgets.hovered.bg_fill
            },
        );
    }
    let folder_color = if ui.visuals().dark_mode {
        egui::Color32::from_rgb(241, 193, 89)
    } else {
        egui::Color32::from_rgb(143, 96, 12)
    };
    let color = if active {
        ui.visuals().selection.stroke.color
    } else if entry.directory {
        folder_color
    } else {
        ui.visuals().text_color()
    };
    composer_icons::image(
        if entry.directory {
            Icon::Folder
        } else {
            Icon::File
        },
        color,
    )
    .paint_at(
        ui,
        egui::Rect::from_center_size(
            egui::pos2(rect.left() + 11.0, rect.center().y),
            egui::vec2(14.0, 14.0),
        ),
    );
    let metadata = if entry.directory {
        "文件夹".into()
    } else {
        format!("{} · {}", file_kind(&entry.name), size(entry.size))
    };
    let font = egui::TextStyle::Body.resolve(ui.style());
    let metadata_width = (rect.width() * 0.43).min(135.0);
    let name_width = (rect.width() - metadata_width - 32.0).max(10.0);
    for (text, x, width, text_color) in [
        (&entry.name, rect.left() + 24.0, name_width, color),
        (
            &metadata,
            rect.right() - metadata_width,
            metadata_width - 4.0,
            if active || entry.directory {
                color
            } else {
                ui.visuals().weak_text_color()
            },
        ),
    ] {
        let mut job = egui::text::LayoutJob::simple(text.clone(), font.clone(), text_color, width);
        job.wrap.max_rows = 1;
        let galley = ui.painter().layout_job(job);
        ui.painter().galley(
            egui::pos2(x, rect.center().y - galley.size().y / 2.0),
            galley,
            text_color,
        );
    }
    response
}
fn display_local_path(path: &std::path::Path) -> String {
    let text = path.to_string_lossy();
    if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else {
        text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned()
    }
}
fn file_kind(name: &str) -> String {
    std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| e.len() <= 8)
        .map(|e| e.to_uppercase())
        .unwrap_or_else(|| "文件".into())
}
fn matches_filter(state: &State, filter: usize) -> bool {
    match filter {
        1 => matches!(state, State::Waiting | State::Running | State::Pausing),
        2 => *state == State::Done,
        3 => *state == State::Failed,
        4 => *state == State::Paused,
        _ => true,
    }
}
fn state_style(state: &State, dark: bool) -> (&'static str, egui::Color32) {
    let (label, light, night) = match state {
        State::Running => ("传输中", [24, 90, 174], [114, 177, 255]),
        State::Done => ("已完成", [22, 117, 66], [99, 214, 147]),
        State::Failed => ("失败", [183, 41, 41], [255, 133, 133]),
        State::Paused => ("已暂停", [140, 94, 8], [237, 193, 95]),
        State::Pausing => ("正在暂停", [140, 94, 8], [237, 193, 95]),
        State::Waiting => ("等待中", [91, 100, 112], [176, 184, 198]),
    };
    let c = if dark { night } else { light };
    (label, egui::Color32::from_rgb(c[0], c[1], c[2]))
}
fn split_handle(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    salt: &str,
    vertical: bool,
    bounds: egui::Rect,
    ratio: &mut f32,
) {
    let response = ui
        .interact(rect, ui.scope_id().with(salt), egui::Sense::drag())
        .on_hover_cursor(if vertical {
            egui::CursorIcon::ResizeHorizontal
        } else {
            egui::CursorIcon::ResizeVertical
        });
    if response.dragged()
        && let Some(pos) = response.interact_pointer_pos()
    {
        *ratio = if vertical {
            ((pos.x - bounds.left()) / bounds.width()).clamp(
                (240.0 / bounds.width()).min(0.49),
                1.0 - (240.0 / bounds.width()).min(0.49),
            )
        } else {
            ((pos.y - bounds.top()) / bounds.height()).clamp(0.40, 0.78)
        };
    }
    let color = if response.hovered() || response.dragged() {
        ui.visuals().selection.bg_fill
    } else {
        ui.visuals().widgets.noninteractive.bg_stroke.color
    };
    let points = if vertical {
        [
            egui::pos2(rect.center().x, rect.top()),
            egui::pos2(rect.center().x, rect.bottom()),
        ]
    } else {
        [
            egui::pos2(rect.left(), rect.center().y),
            egui::pos2(rect.right(), rect.center().y),
        ]
    };
    ui.painter()
        .line_segment(points, egui::Stroke::new(2.0, color));
}
fn size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}
impl Browser {
    fn action_dialog(
        &mut self,
        ctx: &egui::Context,
        runtime: &tokio::runtime::Handle,
        connection: Option<&Connection>,
        title: &str,
    ) {
        if self.action.as_ref().is_some_and(|a| a.remote) && connection.is_none() {
            self.action = None;
        }
        let Some(action) = &mut self.action else {
            return;
        };
        let mut confirm = false;
        let mut cancel = false;
        let response = egui::Modal::new(egui::Id::unique("sftp-file-action")).show(ctx, |ui| {
            ui.set_max_width(440.0);
            ui.heading(if action.new_name.is_some() {
                "重命名"
            } else {
                "确认删除"
            });
            ui.label(if action.remote {
                format!("远程主机：{title}")
            } else {
                "本地电脑".into()
            });
            ui.label(&action.path);
            let valid = if let Some(name) = &mut action.new_name {
                ui.text_edit_singleline(name);
                let valid = transfer::safe_name(name).is_ok();
                if !valid {
                    ui.colored_label(ui.visuals().error_fg_color, "请输入有效的单个文件名");
                }
                valid
            } else {
                ui.label("永久删除，不进入回收站。文件夹及其全部内容将递归删除。此操作无法撤销。");
                true
            };
            ui.horizontal(|ui| {
                cancel = ui.button("取消").clicked();
                confirm = ui
                    .add_enabled(
                        valid && (!action.remote || connection.is_some()),
                        egui::Button::new(if action.new_name.is_some() {
                            "保存名称"
                        } else {
                            "确认删除"
                        }),
                    )
                    .clicked();
            });
        });
        if response.should_close() || cancel {
            self.action = None;
            return;
        }
        if confirm {
            let action = self.action.take().unwrap();
            let (tx, rx) = oneshot::channel();
            let remote = action.remote;
            if remote {
                let result = connection
                    .ok_or_else(|| anyhow::anyhow!("会话已断开"))
                    .and_then(|c| {
                        transfer::start(
                            runtime,
                            c,
                            Request::Manage {
                                path: action.path,
                                new_name: action.new_name,
                            },
                        )
                    });
                match result {
                    Ok(mut job) => {
                        runtime.spawn(async move {
                            let r = (&mut job.result)
                                .await
                                .unwrap_or_else(|_| Err("操作中断".into()))
                                .map(|_| ());
                            let _ = tx.send(r);
                        });
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e.to_string()));
                    }
                }
                self.status = "正在操作…".into();
            } else {
                runtime.spawn(async move {
                    let r = transfer::manage_local(PathBuf::from(action.path), action.new_name)
                        .await
                        .map_err(|e| e.to_string());
                    let _ = tx.send(r);
                });
                self.local_status = "正在操作…".into();
            }
            self.mutation = Some((remote, rx));
        }
    }
    pub fn smoke_fixture(&mut self) {
        self.open = true;
        self.initialized = true;
        self.path = "/home/demo".into();
        self.remote_dir = self.path.clone();
        self.local_path = r"C:\Users\Demo\Desktop".into();
        self.local_dir = PathBuf::from(&self.local_path);
        for entries in [&mut self.entries, &mut self.local_entries] {
            *entries = vec![
                Entry {
                    name: "documents".into(),
                    directory: true,
                    size: 0,
                },
                Entry {
                    name: "备份文件夹".into(),
                    directory: true,
                    size: 0,
                },
                Entry {
                    name: "server-config.toml".into(),
                    directory: false,
                    size: 4182,
                },
                Entry {
                    name: "application-2026-11-05.log".into(),
                    directory: false,
                    size: 8128342,
                },
            ];
        }
        self.status = "4 项 · 窗口测试数据".into();
        self.local_status = self.status.clone();
        self.enqueue(DragEntry {
            remote: false,
            path: r"C:\Users\Demo\Desktop\备份文件夹".into(),
            name: "备份文件夹".into(),
        });
        self.tasks[0].state = State::Paused;
        self.tasks[0].bytes = 1048576;
        for (name, state) in [
            ("server-config.toml", State::Done),
            ("application.log", State::Running),
            ("backup.zip", State::Failed),
        ] {
            self.enqueue(DragEntry {
                remote: true,
                path: format!("/home/demo/{name}"),
                name: name.into(),
            });
            let task = self.tasks.last_mut().unwrap();
            task.state = state;
            task.bytes = 4096;
            if task.state == State::Failed {
                task.detail = "演示：目标目录无写入权限，可重试".into();
            }
        }
    }
    fn list_remote(
        &mut self,
        runtime: &tokio::runtime::Handle,
        connection: Option<&Connection>,
        path: String,
    ) {
        match connection
            .ok_or_else(|| anyhow::anyhow!("请先连接 SSH 会话"))
            .and_then(|c| transfer::start(runtime, c, Request::List(path)))
        {
            Ok(job) => {
                self.listing = Some(job);
                self.status = "正在读取目录…".into();
            }
            Err(e) => self.status = e.to_string(),
        }
    }
    fn list_local(&mut self, runtime: &tokio::runtime::Handle, path: PathBuf) {
        let (tx, rx) = oneshot::channel();
        self.local_listing = Some(rx);
        self.local_status = "正在读取目录…".into();
        runtime.spawn(async move {
            let result: anyhow::Result<_> = async {
                let path = if path.as_os_str().is_empty() {
                    tokio::task::spawn_blocking(|| {
                        directories::UserDirs::new()
                            .and_then(|d| d.desktop_dir().map(PathBuf::from))
                            .ok_or_else(|| anyhow::anyhow!("无法定位系统桌面，请输入本地目录"))
                    })
                    .await??
                } else {
                    path
                };
                let path = tokio::fs::canonicalize(path).await?;
                let mut dir = tokio::fs::read_dir(&path).await?;
                let mut entries = vec![];
                while let Some(entry) = dir.next_entry().await? {
                    if entries.len() >= 10000 {
                        anyhow::bail!("目录超过 10000 项，请选择更具体的路径");
                    }
                    let meta = entry.metadata().await?;
                    entries.push(Entry {
                        name: entry.file_name().to_string_lossy().into_owned(),
                        directory: meta.is_dir(),
                        size: meta.len(),
                    });
                }
                entries.sort_by(|a, b| {
                    b.directory
                        .cmp(&a.directory)
                        .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                });
                Ok((path, entries))
            }
            .await;
            let _ = tx.send(result.map_err(|e| e.to_string()));
        });
    }
    fn enqueue(&mut self, drag: DragEntry) {
        if self.tasks.len() >= MAX_TASKS {
            self.status = "队列已达 500 项，请清理已完成任务".into();
            return;
        }
        if let Err(e) = transfer::safe_name(&drag.name) {
            self.status = e.to_string();
            return;
        }
        let request = if drag.remote {
            Request::Download {
                remote: drag.path,
                local: self.local_dir.join(&drag.name),
            }
        } else {
            Request::Upload {
                local: PathBuf::from(drag.path),
                remote: format!("{}/{}", self.remote_dir.trim_end_matches('/'), drag.name),
            }
        };
        self.tasks.push(Task {
            request,
            resume: Resume::default(),
            name: drag.name,
            state: State::Waiting,
            job: None,
            bytes: 0,
            detail: String::new(),
        });
    }
    fn poll(&mut self, runtime: &tokio::runtime::Handle, connection: Option<&Connection>) {
        let result = self
            .mutation
            .as_mut()
            .and_then(|(remote, rx)| match rx.try_recv() {
                Ok(r) => Some((*remote, r)),
                Err(oneshot::error::TryRecvError::Closed) => {
                    Some((*remote, Err("操作中断".into())))
                }
                _ => None,
            });
        if let Some((remote, result)) = result {
            self.mutation = None;
            match result {
                Ok(()) => {
                    if remote {
                        self.list_remote(runtime, connection, self.remote_dir.clone());
                    } else {
                        self.list_local(runtime, self.local_dir.clone());
                    }
                }
                Err(e) => {
                    let status = if remote {
                        &mut self.status
                    } else {
                        &mut self.local_status
                    };
                    *status = format!("操作失败（可能部分完成，请刷新）：{e}");
                }
            }
        }
        if let Some(job) = &mut self.listing {
            match job.result.try_recv() {
                Ok(result) => {
                    match result {
                        Ok(Outcome::Listed { path, entries }) => {
                            self.remote_dir = path.clone();
                            self.path = path;
                            self.entries = entries;
                            self.remote_selected = None;
                            self.status = format!("{} 项", self.entries.len());
                        }
                        Err(e) => self.status = e,
                        _ => {}
                    }
                    self.listing = None;
                }
                Err(oneshot::error::TryRecvError::Closed) => {
                    self.listing = None;
                    self.status = "目录读取已中断".into();
                }
                _ => {}
            }
        }
        if let Some(rx) = &mut self.local_listing {
            match rx.try_recv() {
                Ok(result) => {
                    match result {
                        Ok((path, entries)) => {
                            self.local_path = display_local_path(&path);
                            self.local_dir = path;
                            self.local_entries = entries;
                            self.local_selected = None;
                            self.local_status = format!("{} 项", self.local_entries.len());
                        }
                        Err(e) => self.local_status = e,
                    }
                    self.local_listing = None;
                }
                Err(oneshot::error::TryRecvError::Closed) => {
                    self.local_listing = None;
                    self.local_status = "目录读取已中断".into();
                }
                _ => {}
            }
        }
        let mut refresh = false;
        for task in &mut self.tasks {
            if let Some(job) = &mut task.job {
                task.bytes = *job.progress.borrow();
                let result = match job.result.try_recv() {
                    Ok(r) => Some(r),
                    Err(oneshot::error::TryRecvError::Closed) => {
                        Some(Err("传输中断，可重试续传".into()))
                    }
                    _ => None,
                };
                if let Some(result) = result {
                    match result {
                        Ok(_) => {
                            task.state = State::Done;
                            task.detail.clear();
                            refresh = true;
                        }
                        Err(e) => {
                            task.state = if task.state == State::Pausing {
                                State::Paused
                            } else {
                                State::Failed
                            };
                            task.detail = e;
                        }
                    }
                    task.job = None;
                }
            }
        }
        if refresh {
            self.list_local(runtime, self.local_dir.clone());
            self.list_remote(runtime, connection, self.remote_dir.clone());
        }
        if !self.tasks.iter().any(|t| t.job.is_some())
            && let Some(c) = connection
            && let Some(task) = self.tasks.iter_mut().find(|t| t.state == State::Waiting)
        {
            match transfer::start_resumable(runtime, c, task.request.clone(), task.resume.clone()) {
                Ok(job) => {
                    task.job = Some(job);
                    task.state = State::Running;
                    task.detail.clear();
                }
                Err(e) => {
                    task.state = State::Failed;
                    task.detail = e.to_string();
                }
            }
        }
    }
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        runtime: &tokio::runtime::Handle,
        connection: Option<&Connection>,
        id: uuid::Uuid,
        title: &str,
    ) {
        self.poll(runtime, connection);
        if self.tasks.iter().any(|t| t.job.is_some())
            || self.listing.is_some()
            || self.local_listing.is_some()
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        if !self.open {
            return;
        }
        if !self.initialized {
            self.initialized = true;
            self.list_local(runtime, PathBuf::new());
            self.list_remote(runtime, connection, self.path.clone());
        }
        let mut open = self.open;
        let mut remote_nav = None;
        let mut local_nav = None;
        let mut enqueue = vec![];
        let mut requested_action = None;
        let can_manage = self.mutation.is_none();
        let mut content = |ui: &mut egui::Ui| {
            ui.weak("双击打开文件夹 · 右键重命名 / 删除 · 拖到另一栏传输");
            let available = ui.available_size();
            let height = available.y.max(420.0);
            let top_height = (height * self.queue_split).clamp(230.0, height - 150.0);
            let (area, _) =
                ui.allocate_exact_size(egui::vec2(available.x, top_height), egui::Sense::hover());
            let split_x = area.left() + area.width() * self.column_split;
            let divider = egui::Rect::from_min_max(
                egui::pos2(split_x - 4.0, area.top()),
                egui::pos2(split_x + 4.0, area.bottom()),
            );
            split_handle(ui, divider, "columns", true, area, &mut self.column_split);
            for index in 0..2 {
                let remote = index == 0;
                let pane = if remote {
                    egui::Rect::from_min_max(area.min, egui::pos2(split_x - 5.0, area.bottom()))
                } else {
                    egui::Rect::from_min_max(egui::pos2(split_x + 5.0, area.top()), area.max)
                };
                ui.scope_builder(
                    egui::UiBuilder::new()
                        .id_salt(index)
                        .max_rect(pane)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                    |ui| {
                        ui.set_clip_rect(pane.intersect(ui.clip_rect()));
                        ui.strong(if remote {
                            "远程主机"
                        } else {
                            "本地电脑"
                        });
                        let path = if remote {
                            &mut self.path
                        } else {
                            &mut self.local_path
                        };
                        let edit =
                            ui.add(egui::TextEdit::singleline(path).desired_width(f32::INFINITY));
                        let mut refresh =
                            edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        ui.horizontal_wrapped(|ui| {
                            refresh |= ui.button("打开 / 刷新").clicked();
                            if ui.button("上级目录").clicked() {
                                if remote {
                                    remote_nav = Some(format!(
                                        "{}/..",
                                        self.remote_dir.trim_end_matches('/')
                                    ));
                                } else if let Some(parent) = self.local_dir.parent() {
                                    local_nav = Some(parent.to_path_buf());
                                }
                            }
                            if !remote && ui.button("桌面").clicked() {
                                local_nav = Some(PathBuf::new());
                            }
                            let selected = if remote {
                                self.remote_selected
                            } else {
                                self.local_selected
                            };
                            if ui
                                .add_enabled(
                                    selected.is_some(),
                                    egui::Button::new(if remote { "下载" } else { "上传" }),
                                )
                                .clicked()
                            {
                                let entries = if remote {
                                    &self.entries
                                } else {
                                    &self.local_entries
                                };
                                if let Some(entry) = selected.and_then(|i| entries.get(i)) {
                                    enqueue.push(DragEntry {
                                        remote,
                                        name: entry.name.clone(),
                                        path: if remote {
                                            format!(
                                                "{}/{}",
                                                self.remote_dir.trim_end_matches('/'),
                                                entry.name
                                            )
                                        } else {
                                            self.local_dir
                                                .join(&entry.name)
                                                .to_string_lossy()
                                                .into_owned()
                                        },
                                    });
                                }
                            }
                        });
                        if refresh {
                            if remote {
                                remote_nav = Some(self.path.clone());
                            } else {
                                local_nav = Some(PathBuf::from(&self.local_path));
                            }
                        }
                        ui.horizontal(|ui| {
                            let changed = ui
                                .add(
                                    egui::TextEdit::singleline(&mut self.name_filters[index])
                                        .hint_text("查找当前目录文件名…")
                                        .desired_width((ui.available_width() - 48.0).max(60.0)),
                                )
                                .changed();
                            let clear = ui
                                .add_enabled(
                                    !self.name_filters[index].is_empty(),
                                    egui::Button::new("清除"),
                                )
                                .clicked();
                            if clear {
                                self.name_filters[index].clear();
                            }
                            if changed || clear {
                                if remote {
                                    self.remote_selected = None;
                                } else {
                                    self.local_selected = None;
                                }
                            }
                        });
                        let entries = if remote {
                            &self.entries
                        } else {
                            &self.local_entries
                        };
                        let visible = visible_entries(entries, &self.name_filters[index]);
                        let selected = if remote {
                            &mut self.remote_selected
                        } else {
                            &mut self.local_selected
                        };
                        let browser_height = (pane.bottom() - ui.cursor().top() - 38.0).max(70.0);
                        let panel = egui::Frame::group(ui.style())
                            .show(ui, |ui| {
                                ui.set_min_height(browser_height);
                                ui.set_min_width((ui.available_width() - 2.0).max(0.0));
                                ui.horizontal(|ui| {
                                    ui.weak("名称");
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            ui.weak("大小 / 类型");
                                        },
                                    );
                                });
                                ui.separator();
                                egui::ScrollArea::vertical()
                                    .id_salt("files")
                                    .max_height(browser_height - 30.0)
                                    .show_rows(ui, 25.0, visible.len(), |ui, range| {
                                        for visible_index in range {
                                            let i = visible[visible_index];
                                            let entry = &entries[i];
                                            let response =
                                                file_row(ui, entry, *selected == Some(i), i);
                                            if response.clicked() {
                                                *selected = Some(i);
                                            }
                                            let path = if remote {
                                                format!(
                                                    "{}/{}",
                                                    self.remote_dir.trim_end_matches('/'),
                                                    entry.name
                                                )
                                            } else {
                                                self.local_dir
                                                    .join(&entry.name)
                                                    .to_string_lossy()
                                                    .into_owned()
                                            };
                                            response.context_menu(|ui| {
                                                for (label, rename) in
                                                    [("重命名…", true), ("删除…", false)]
                                                {
                                                    if ui
                                                        .add_enabled(
                                                            can_manage
                                                                && transfer::safe_name(&entry.name)
                                                                    .is_ok(),
                                                            egui::Button::new(label),
                                                        )
                                                        .clicked()
                                                    {
                                                        *selected = Some(i);
                                                        requested_action = Some(FileAction {
                                                            remote,
                                                            path: path.clone(),
                                                            new_name: rename
                                                                .then(|| entry.name.clone()),
                                                        });
                                                        ui.close();
                                                    }
                                                }
                                            });
                                            response.dnd_set_drag_payload(DragEntry {
                                                remote,
                                                path: path.clone(),
                                                name: entry.name.clone(),
                                            });
                                            if response.double_clicked() && entry.directory {
                                                if remote {
                                                    remote_nav = Some(path);
                                                } else {
                                                    local_nav = Some(PathBuf::from(path));
                                                }
                                            }
                                        }
                                    });
                                if visible.is_empty() && !entries.is_empty() {
                                    ui.weak("没有匹配的文件或文件夹");
                                }
                                if entries.is_empty() {
                                    ui.weak("目录为空或尚未加载");
                                }
                            })
                            .response;
                        if let Some(drag) = panel.dnd_hover_payload::<DragEntry>()
                            && drag.remote != remote
                        {
                            ui.painter().rect_stroke(
                                panel.rect,
                                3.0,
                                egui::Stroke::new(2.0, ui.visuals().selection.bg_fill),
                                egui::StrokeKind::Inside,
                            );
                        }
                        if let Some(drag) = panel.dnd_release_payload::<DragEntry>()
                            && drag.remote != remote
                        {
                            enqueue.push((*drag).clone());
                        }
                        if remote
                            && ui.input(|i| {
                                i.pointer
                                    .hover_pos()
                                    .is_some_and(|p| panel.rect.contains(p))
                            })
                        {
                            for file in ui.input(|i| i.raw.dropped_files.clone()) {
                                let path = file.path();
                                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                                    enqueue.push(DragEntry {
                                        remote: false,
                                        path: path.to_string_lossy().into_owned(),
                                        name: name.into(),
                                    });
                                }
                            }
                        }
                        if !self.name_filters[index].is_empty() {
                            ui.label(format!("匹配 {} / {} 项", visible.len(), entries.len()));
                        } else {
                            ui.label(if remote {
                                &self.status
                            } else {
                                &self.local_status
                            });
                        }
                    },
                );
            }
            let (divider, _) =
                ui.allocate_exact_size(egui::vec2(available.x, 8.0), egui::Sense::hover());
            let bounds = egui::Rect::from_min_size(area.min, egui::vec2(available.x, height));
            split_handle(
                ui,
                divider,
                "queue-divider",
                false,
                bounds,
                &mut self.queue_split,
            );
            ui.horizontal(|ui| {
                ui.strong(format!("传输队列  {} / {MAX_TASKS}", self.tasks.len()));
                if ui.button("全部暂停").clicked() {
                    for t in &mut self.tasks {
                        if let Some(job) = &t.job {
                            job.cancel();
                            t.state = State::Pausing;
                        } else if t.state == State::Waiting {
                            t.state = State::Paused;
                        }
                    }
                }
                if ui.button("清理已完成").clicked() {
                    self.tasks.retain(|t| t.state != State::Done);
                }
            });
            ui.horizontal_wrapped(|ui| {
                for (index, label) in ["全部", "进行中", "已完成", "失败", "已暂停"]
                    .into_iter()
                    .enumerate()
                {
                    let count = self
                        .tasks
                        .iter()
                        .filter(|t| matches_filter(&t.state, index))
                        .count();
                    ui.selectable_value(&mut self.task_filter, index, format!("{label} ({count})"));
                }
            });
            let mut remove = None;
            egui::ScrollArea::vertical()
                .id_salt("queue")
                .auto_shrink([false, false])
                .max_height((height - top_height - 100.0).max(50.0))
                .show(ui, |ui| {
                    if self.tasks.is_empty() {
                        ui.weak("暂无任务。选择文件或文件夹后上传 / 下载，或直接拖动。");
                    }
                    for (index, task) in self.tasks.iter_mut().enumerate() {
                        if !matches_filter(&task.state, self.task_filter) {
                            continue;
                        }
                        ui.push_id(index, |ui| {
                            let (label, color) = state_style(&task.state, ui.visuals().dark_mode);
                            egui::Frame::new()
                                .fill(color.gamma_multiply(0.09))
                                .inner_margin(4.0)
                                .show(ui, |ui| {
                                    ui.set_min_width(ui.available_width());
                                    ui.horizontal(|ui| {
                                        let (direction, tooltip) = match &task.request {
                                            Request::Upload { local, remote } => {
                                                ("上传", format!("{} → {remote}", local.display()))
                                            }
                                            Request::Download { local, remote } => {
                                                ("下载", format!("{remote} → {}", local.display()))
                                            }
                                            _ => unreachable!(),
                                        };
                                        egui::Frame::new()
                                            .fill(color.gamma_multiply(0.18))
                                            .corner_radius(3.0)
                                            .inner_margin(3.0)
                                            .show(ui, |ui| {
                                                ui.label(
                                                    egui::RichText::new(label)
                                                        .strong()
                                                        .color(color),
                                                );
                                            });
                                        if task.state == State::Running {
                                            ui.spinner();
                                        }
                                        ui.label(direction);
                                        ui.add_sized(
                                            egui::vec2(
                                                (ui.available_width() * 0.32).min(280.0),
                                                20.0,
                                            ),
                                            egui::Label::new(&task.name)
                                                .truncate()
                                                .halign(egui::Align::Min),
                                        )
                                        .on_hover_text(tooltip);
                                        ui.label(size(task.bytes));

                                        if task.state == State::Running
                                            && ui.small_button("暂停").clicked()
                                        {
                                            if let Some(job) = &task.job {
                                                job.cancel();
                                            }
                                            task.state = State::Pausing;
                                        }
                                        if matches!(task.state, State::Paused | State::Failed)
                                            && ui.small_button("继续").clicked()
                                        {
                                            task.state = State::Waiting;
                                        }
                                        if task.job.is_none()
                                            && ui
                                                .small_button("移除")
                                                .on_hover_text("移除队列记录；保留未完成的临时文件")
                                                .clicked()
                                        {
                                            remove = Some(index);
                                        }
                                    });
                                    if !task.detail.is_empty() {
                                        ui.colored_label(color, &task.detail);
                                    }
                                });
                            ui.add_space(3.0);
                        });
                    }
                });
            if let Some(i) = remove {
                self.tasks.remove(i);
            }
            if let Some(action) = requested_action.take() {
                self.action = Some(action);
            }
            self.action_dialog(ui.ctx(), runtime, connection, title);
            ui.weak("同名文件不覆盖；关闭面板后台继续。续传记录仅保留在当前会话，关闭会话或退出程序后不保留。");
        };
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of(("sftp-native", id)),
            egui::ViewportBuilder::default()
                .with_title(format!("SFTP · {title}"))
                .with_inner_size([1000.0, 630.0])
                .with_min_inner_size([580.0, 480.0])
                .with_resizable(true),
            |ui, class| {
                if ui.input(|i| i.viewport().close_requested()) {
                    open = false;
                }
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(100));
                if class == egui::ViewportClass::EmbeddedWindow {
                    content(ui);
                } else {
                    egui::CentralPanel::default().show(ui, &mut content);
                }
            },
        );
        self.open = open;
        if !open {
            self.action = None;
        }
        if let Some(path) = remote_nav {
            self.list_remote(runtime, connection, path);
        }
        if let Some(path) = local_nav {
            self.list_local(runtime, path);
        }
        for drag in enqueue {
            self.enqueue(drag);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn drops_capture_both_directories_and_reject_unsafe_names() {
        let mut browser = Browser {
            remote_dir: "/srv".into(),
            local_dir: PathBuf::from(r"C:\Downloads"),
            ..Default::default()
        };
        browser.enqueue(DragEntry {
            remote: false,
            path: r"C:\source\folder".into(),
            name: "folder".into(),
        });
        browser.enqueue(DragEntry {
            remote: true,
            path: "/srv/file.txt".into(),
            name: "file.txt".into(),
        });
        browser.enqueue(DragEntry {
            remote: true,
            path: "/srv/../bad".into(),
            name: "../bad".into(),
        });
        assert_eq!(browser.tasks.len(), 2);
        assert!(
            matches!(&browser.tasks[0].request, Request::Upload { remote, .. } if remote == "/srv/folder")
        );
        assert!(
            matches!(&browser.tasks[1].request, Request::Download { local, .. } if local == &PathBuf::from(r"C:\Downloads\file.txt"))
        );
        browser.remote_dir = "/other".into();
        assert!(
            matches!(&browser.tasks[0].request, Request::Upload { remote, .. } if remote == "/srv/folder")
        );
    }
    #[test]
    fn dual_panes_and_queue_render_in_both_themes_and_narrow_window() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        for light in [false, true] {
            for width in [800.0, 1180.0] {
                let ctx = egui::Context::default();
                ctx.set_visuals(if light {
                    egui::Visuals::light()
                } else {
                    egui::Visuals::dark()
                });
                egui_extras::install_image_loaders(&ctx);
                let mut browser = Browser::default();
                browser.smoke_fixture();
                let id = uuid::Uuid::new_v4();
                for frame in 0..3 {
                    let mut output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(width, 780.0),
                            )),
                            ..Default::default()
                        },
                        |_| browser.show(&ctx, runtime.handle(), None, id, "test"),
                    );
                    output.textures_delta.clear();
                    if frame < 2 {
                        continue;
                    }
                    for label in ["远程主机", "本地电脑", "全部暂停", "继续"] {
                        let text = output
                            .shapes
                            .iter()
                            .find_map(|s| composer_icons::find_text(&s.shape, label))
                            .unwrap_or_else(|| panic!("missing {label}"));
                        let rect = text.visual_bounding_rect();
                        assert!(
                            rect.min.x >= 0.0 && rect.max.x <= width && rect.max.y <= 780.0,
                            "clipped {label}: {rect:?}"
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn dragging_files_and_folders_between_panes_creates_tasks() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let ctx = egui::Context::default();
        egui_extras::install_image_loaders(&ctx);
        let mut browser = Browser::default();
        browser.smoke_fixture();
        browser.tasks.clear();
        let id = uuid::Uuid::new_v4();
        let frame = |browser: &mut Browser, events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1180.0, 780.0),
                    )),
                    focused: true,
                    events,
                    ..Default::default()
                },
                |_| browser.show(&ctx, runtime.handle(), None, id, "test"),
            );
            output.textures_delta.clear();
            output
        };
        for _ in 0..3 {
            frame(&mut browser, vec![]);
        }
        for name in ["documents", "server-config.toml"] {
            let output = frame(&mut browser, vec![]);
            let positions: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|s| {
                    composer_icons::find_text(&s.shape, name)
                        .map(|t| t.visual_bounding_rect().center())
                })
                .collect();
            assert_eq!(positions.len(), 2);
            let mut positions = positions;
            positions.sort_by(|a, b| a.x.total_cmp(&b.x));
            let button = |pos, pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            for (start, end) in [(positions[0], positions[1]), (positions[1], positions[0])] {
                frame(&mut browser, vec![egui::Event::PointerMoved(start)]);
                frame(&mut browser, vec![button(start, true)]);
                frame(&mut browser, vec![egui::Event::PointerMoved(end)]);
                frame(&mut browser, vec![]);
                frame(&mut browser, vec![button(end, false)]);
            }
        }
        assert_eq!(browser.tasks.len(), 4);
        assert!(matches!(browser.tasks[0].request, Request::Download { .. }));
        assert!(matches!(browser.tasks[1].request, Request::Upload { .. }));
    }
    #[test]
    fn both_split_handles_resize_with_pointer_drag() {
        for vertical in [true, false] {
            let ctx = egui::Context::default();
            let mut ratio = 0.5;
            let bounds =
                egui::Rect::from_min_size(egui::pos2(20.0, 20.0), egui::vec2(600.0, 500.0));
            let start = if vertical {
                egui::pos2(320.0, 200.0)
            } else {
                egui::pos2(200.0, 270.0)
            };
            let end = start
                + if vertical {
                    egui::vec2(60.0, 0.0)
                } else {
                    egui::vec2(0.0, 50.0)
                };
            let button = |pressed| egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            for events in [
                vec![],
                vec![egui::Event::PointerMoved(start)],
                vec![button(true)],
                vec![egui::Event::PointerMoved(end)],
            ] {
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(800.0, 700.0),
                        )),
                        events,
                        ..Default::default()
                    },
                    |root| {
                        egui::CentralPanel::default().show(root, |ui| {
                            let rect = if vertical {
                                egui::Rect::from_min_max(
                                    egui::pos2(316.0, 20.0),
                                    egui::pos2(324.0, 520.0),
                                )
                            } else {
                                egui::Rect::from_min_max(
                                    egui::pos2(20.0, 266.0),
                                    egui::pos2(620.0, 274.0),
                                )
                            };
                            split_handle(ui, rect, "test", vertical, bounds, &mut ratio);
                        });
                    },
                );
                output.textures_delta.clear();
            }
            assert!((ratio - 0.6).abs() < 0.01, "{vertical}: {ratio}");
        }
    }
    #[test]
    fn task_filters_separate_active_completed_and_failed() {
        assert!(matches_filter(&State::Running, 1));
        assert!(!matches_filter(&State::Done, 1));
        assert!(matches_filter(&State::Done, 2));
        assert!(matches_filter(&State::Failed, 3));
        assert!(!matches_filter(&State::Paused, 3));
        assert_eq!(
            display_local_path(std::path::Path::new(r"\\?\C:\Users\demo\Desktop")),
            r"C:\Users\demo\Desktop"
        );
    }
    #[test]
    fn clicking_icon_name_and_metadata_selects_whole_row() {
        let entry = Entry {
            name: "example.txt".into(),
            directory: false,
            size: 1024,
        };
        for dark in [false, true] {
            let ctx = egui::Context::default();
            egui_extras::install_image_loaders(&ctx);
            ctx.set_visuals(if dark {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            });
            let mut selected = false;
            let frame = |events, selected: &mut bool| {
                let mut rect = egui::Rect::NOTHING;
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(400.0, 200.0),
                        )),
                        events,
                        ..Default::default()
                    },
                    |root| {
                        egui::CentralPanel::default().show(root, |ui| {
                            let r = file_row(ui, &entry, *selected, 0);
                            rect = r.rect;
                            if r.clicked() {
                                *selected = true;
                            }
                        });
                    },
                );
                output.textures_delta.clear();
                rect
            };
            let rect = frame(vec![], &mut selected);
            for x in [rect.left() + 10.0, rect.left() + 70.0, rect.right() - 20.0] {
                selected = false;
                let pos = egui::pos2(x, rect.center().y);
                frame(vec![egui::Event::PointerMoved(pos)], &mut selected);
                for pressed in [true, false] {
                    frame(
                        vec![egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        }],
                        &mut selected,
                    );
                }
                assert!(selected, "click x={x} dark={dark}");
            }
        }
    }
    #[test]
    fn filename_search_maps_results_to_original_entries() {
        let mut browser = Browser::default();
        browser.smoke_fixture();
        assert_eq!(visible_entries(&browser.entries, "TOML"), vec![2]);
        assert_eq!(visible_entries(&browser.entries, "备份"), vec![1]);
        assert!(visible_entries(&browser.entries, "nonexistent").is_empty());
        assert_eq!(visible_entries(&browser.entries, ""), vec![0, 1, 2, 3]);
        browser.name_filters = ["TOML".into(), "LOG".into()];
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let ctx = egui::Context::default();
        egui_extras::install_image_loaders(&ctx);
        let id = uuid::Uuid::new_v4();
        let frame = |browser: &mut Browser, events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1180.0, 780.0),
                    )),
                    events,
                    ..Default::default()
                },
                |_| browser.show(&ctx, runtime.handle(), None, id, "test"),
            );
            output.textures_delta.clear();
            output
        };
        for _ in 0..3 {
            frame(&mut browser, vec![]);
        }
        let output = frame(&mut browser, vec![]);
        let pos = output
            .shapes
            .iter()
            .find_map(|s| composer_icons::find_text(&s.shape, "TOML · 4.1 KB"))
            .unwrap()
            .visual_bounding_rect()
            .center();
        frame(&mut browser, vec![egui::Event::PointerMoved(pos)]);
        for pressed in [true, false] {
            frame(
                &mut browser,
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
        }
        assert_eq!(browser.remote_selected, Some(2));
        assert_eq!(browser.local_selected, None);
    }
}

#[cfg(test)]
mod action_tests {
    use super::*;
    #[test]
    fn deletion_requires_click_and_cancel_does_not_dispatch() {
        for dark in [true, false] {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let ctx = egui::Context::default();
            ctx.set_visuals(if dark {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            });
            let mut browser = Browser {
                action: Some(FileAction {
                    remote: false,
                    path: r"C:\test\example.txt".into(),
                    new_name: None,
                }),
                ..Default::default()
            };
            let frame = |browser: &mut Browser, events| {
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(580.0, 480.0),
                        )),
                        events,
                        ..Default::default()
                    },
                    |_| browser.action_dialog(&ctx, runtime.handle(), None, "test"),
                );
                output.textures_delta.clear();
                output
            };
            for _ in 0..3 {
                frame(&mut browser, vec![]);
            }
            assert!(browser.mutation.is_none());
            let output = frame(&mut browser, vec![]);
            let pos = output
                .shapes
                .iter()
                .find_map(|s| composer_icons::find_text(&s.shape, "取消"))
                .unwrap()
                .visual_bounding_rect()
                .center();
            frame(&mut browser, vec![egui::Event::PointerMoved(pos)]);
            for pressed in [true, false] {
                frame(
                    &mut browser,
                    vec![egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    }],
                );
            }
            assert!(browser.action.is_none());
            assert!(browser.mutation.is_none());
            let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/test-artifacts");
            std::fs::create_dir_all(&root).unwrap();
            let dir = tempfile::tempdir_in(root).unwrap();
            let file = dir.path().join("delete-after-confirm.txt");
            std::fs::write(&file, b"test").unwrap();
            browser.action = Some(FileAction {
                remote: false,
                path: file.to_string_lossy().into_owned(),
                new_name: None,
            });
            for _ in 0..3 {
                frame(&mut browser, vec![]);
            }
            assert!(file.exists());
            let output = frame(&mut browser, vec![]);
            let pos = output
                .shapes
                .iter()
                .filter_map(|s| composer_icons::find_text(&s.shape, "确认删除"))
                .next_back()
                .unwrap()
                .visual_bounding_rect()
                .center();
            frame(&mut browser, vec![egui::Event::PointerMoved(pos)]);
            for pressed in [true, false] {
                frame(
                    &mut browser,
                    vec![egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    }],
                );
            }
            let (_, rx) = browser
                .mutation
                .take()
                .expect("explicit click dispatches deletion");
            runtime.block_on(async {
                tokio::time::timeout(std::time::Duration::from_secs(5), rx)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
            });
            assert!(!file.exists());
        }
    }
}
