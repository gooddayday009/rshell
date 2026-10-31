use eframe::egui;
use rshell_core::{
    connection::Connection,
    transfer::{self, Entry, Job, Outcome, Request},
};
use std::path::PathBuf;
use tokio::sync::oneshot;

pub struct Browser {
    pub open: bool,
    path: String,
    entries: Vec<Entry>,
    status: String,
    job: Option<Job>,
    selection: Option<oneshot::Receiver<Option<(PathBuf, String, bool)>>>,
}
impl Default for Browser {
    fn default() -> Self {
        Self {
            open: false,
            path: ".".into(),
            entries: Vec::new(),
            status: String::new(),
            job: None,
            selection: None,
        }
    }
}
impl Browser {
    fn start(
        &mut self,
        runtime: &tokio::runtime::Handle,
        connection: Option<&Connection>,
        request: Request,
    ) {
        let result = connection
            .ok_or_else(|| anyhow::anyhow!("请先连接 SSH 会话"))
            .and_then(|c| transfer::start(runtime, c, request));
        match result {
            Ok(job) => {
                self.job = Some(job);
                self.status = "处理中…".into();
            }
            Err(e) => self.status = e.to_string(),
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
        if self.open && self.status.is_empty() && connection.is_some() {
            self.start(runtime, connection, Request::List(self.path.clone()));
        }
        if let Some(job) = &mut self.job {
            match job.result.try_recv() {
                Ok(result) => {
                    match result {
                        Ok(Outcome::Listed { path, entries }) => {
                            self.path = path;
                            self.entries = entries;
                            self.status = format!("{} 项", self.entries.len());
                        }
                        Ok(Outcome::Done) => self.status = "传输完成；刷新查看目录".into(),
                        Err(e) => self.status = e,
                    }
                    self.job = None;
                }
                Err(oneshot::error::TryRecvError::Closed) => {
                    self.status = "传输任务已结束".into();
                    self.job = None;
                }
                _ => {}
            }
        }
        if let Some(selection) = &mut self.selection {
            match selection.try_recv() {
                Ok(choice) => {
                    self.selection = None;
                    if let Some((local, remote, upload)) = choice {
                        let request = if upload {
                            Request::Upload { local, remote }
                        } else {
                            Request::Download { local, remote }
                        };
                        self.start(runtime, connection, request);
                    }
                }
                Err(oneshot::error::TryRecvError::Closed) => self.selection = None,
                _ => {}
            }
        }
        if !self.open {
            return;
        }
        let mut open = self.open;
        let mut request = None;
        let mut pick: Option<(String, bool)> = None;
        egui::Window::new(format!("SFTP · {title}")).id(egui::Id::unique(("sftp", id))).open(&mut open).default_width(680.0).anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO).show(ctx, |ui| {
            let busy = self.job.is_some() || self.selection.is_some();
            ui.add_enabled_ui(!busy && connection.is_some(), |ui| {
                ui.horizontal(|ui| {
                    ui.label("远端目录"); ui.text_edit_singleline(&mut self.path);
                    if ui.button("打开 / 刷新").clicked() { request = Some(Request::List(self.path.clone())); }
                    if ui.button("上级").clicked() { request = Some(Request::List(format!("{}/..", self.path.trim_end_matches('/')))); }
                    if ui.button("上传…").clicked() { pick = Some((self.path.clone(), true)); }
                });
                egui::ScrollArea::vertical().min_scrolled_height(230.0).max_height(350.0).show(ui, |ui| {
                    if self.entries.is_empty() { ui.weak("目录为空，或尚未加载目录。"); }
                    for entry in &self.entries {
                        ui.horizontal(|ui| {
                            let remote = format!("{}/{}", self.path.trim_end_matches('/'), entry.name);
                            if entry.directory {
                                if ui.button(format!("📁 {}", entry.name)).clicked() { request = Some(Request::List(remote)); }
                            } else {
                                ui.label(&entry.name); ui.weak(format!("{} B", entry.size));
                                if ui.small_button("下载…").clicked() { pick = Some((remote, false)); }
                            }
                        });
                    }
                });
            });
            if let Some(job) = &self.job {
                ui.horizontal(|ui| {
                    ui.spinner(); ui.label(format!("已传输 {} 字节", *job.progress.borrow()));
                    if ui.button("取消").clicked() { job.cancel(); }
                });
            }
            ui.label(&self.status);
            ui.weak("不覆盖同名文件；失败或取消时 .rshell-*.part 临时文件可能保留。关闭窗口取消当前任务。");
        });
        self.open = open;
        if !open {
            self.job = None;
            self.selection = None;
        }
        if let Some(request) = request {
            self.start(runtime, connection, request);
        }
        if let Some((remote, upload)) = pick {
            let (tx, rx) = oneshot::channel();
            self.selection = Some(rx);
            runtime.spawn(async move {
                let choice = if upload {
                    rfd::AsyncFileDialog::new()
                        .set_title("选择上传到 SFTP 的文件")
                        .pick_file()
                        .await
                        .and_then(|f| {
                            let name = f.path().file_name()?.to_str()?;
                            transfer::safe_name(name).ok()?;
                            Some((
                                f.path().to_owned(),
                                format!("{}/{}", remote.trim_end_matches('/'), name),
                                true,
                            ))
                        })
                } else {
                    let name = remote.rsplit('/').next().unwrap_or("download");
                    rfd::AsyncFileDialog::new()
                        .set_title("选择下载保存位置（已有文件不会覆盖）")
                        .set_file_name(name)
                        .save_file()
                        .await
                        .map(|f| (f.path().to_owned(), remote, false))
                };
                let _ = tx.send(choice);
            });
        }
    }
}
