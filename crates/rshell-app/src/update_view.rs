use super::*;
use rshell_core::update::{self, Package, Release};
use std::sync::atomic::{AtomicU64, Ordering};

enum Outcome {
    Checked(Release),
    Downloaded(PathBuf),
    FolderOpened,
    InstallerStarted,
}

struct Task {
    receiver: std::sync::mpsc::Receiver<Result<Outcome, String>>,
    abort: tokio::task::AbortHandle,
}

#[derive(Default)]
pub(super) struct UpdateState {
    pub open: bool,
    release: Option<Release>,
    downloaded: Option<PathBuf>,
    task: Option<Task>,
    progress: Arc<AtomicU64>,
    total: u64,
    status: String,
    error: String,
    confirm_install: bool,
    pub exit_for_install: bool,
}

impl Drop for UpdateState {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort.abort();
        }
    }
}

impl UpdateState {
    pub fn poll(&mut self) {
        let Some(task) = &self.task else { return };
        match task.receiver.try_recv() {
            Ok(result) => {
                self.task = None;
                match result {
                    Ok(Outcome::Checked(release)) => {
                        self.status = if release.is_newer_than(env!("CARGO_PKG_VERSION")) {
                            "发现新版本".into()
                        } else if release.version == env!("CARGO_PKG_VERSION") {
                            "当前已是最新稳定版本，可重新下载安装包".into()
                        } else {
                            "当前版本高于仓库中的最新稳定版本".into()
                        };
                        self.release = Some(release);
                    }
                    Ok(Outcome::Downloaded(path)) => {
                        self.downloaded = Some(path);
                        self.status = "下载完成，SHA-256 校验通过".into();
                    }
                    Ok(Outcome::FolderOpened) => self.status = "已打开下载目录".into(),
                    Ok(Outcome::InstallerStarted) => self.exit_for_install = true,
                    Err(error) => {
                        self.status.clear();
                        self.error = error;
                    }
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.task = None;
                self.error = "更新任务意外结束，请重试".into();
            }
        }
    }
}

impl App {
    fn update_task(
        &mut self,
        ctx: egui::Context,
        future: impl std::future::Future<Output = anyhow::Result<Outcome>> + Send + 'static,
    ) {
        self.updates.error.clear();
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let task = self.runtime.spawn(async move {
            let result = future.await.map_err(|error| format!("{error:#}"));
            let _ = sender.send(result);
            ctx.request_repaint();
        });
        self.updates.task = Some(Task {
            receiver,
            abort: task.abort_handle(),
        });
    }

    pub(super) fn check_updates(&mut self, ctx: egui::Context) {
        self.updates.open = true;
        if self.updates.task.is_some() {
            return;
        }
        self.updates.total = 0;
        self.updates.status = "正在检查 GitHub 稳定版本…".into();
        self.update_task(ctx, async {
            Ok(Outcome::Checked(update::check_latest().await?))
        });
    }

    fn download_update(&mut self, ctx: egui::Context, package: Package) {
        let root = self.path.parent().unwrap().join("updates");
        self.updates.total = package.size;
        self.updates.progress = Arc::new(AtomicU64::new(0));
        let progress = self.updates.progress.clone();
        self.updates.status = format!("正在下载 {}…", package.name);
        self.updates.downloaded = None;
        self.updates.confirm_install = false;
        self.update_task(ctx, async move {
            let path = update::download(package, root, move |bytes| {
                progress.store(bytes, Ordering::Relaxed);
            })
            .await?;
            Ok(Outcome::Downloaded(path))
        });
    }

    pub(super) fn draw_updates(&mut self, ctx: &egui::Context) {
        if !self.updates.open {
            return;
        }
        let mut check = false;
        let mut download = None;
        let mut folder = None;
        let mut install = None;
        egui::Modal::new(egui::Id::unique("software_update")).show(ctx, |ui| {
            ui.set_max_width(520.0);
            ui.heading("软件更新");
            ui.label(concat!("当前版本：", env!("CARGO_PKG_VERSION")));
            ui.hyperlink_to(
                "GitHub 发布页面 · gooddayday009/rshell",
                update::RELEASES_URL,
            );
            if let Some(release) = &self.updates.release {
                ui.label(format!("最新稳定版本：{}", release.version));
                ui.horizontal_wrapped(|ui| {
                    for package in &release.packages {
                        let label = if package.installer {
                            "下载安装包"
                        } else {
                            "下载便携 ZIP"
                        };
                        if ui
                            .add_enabled(self.updates.task.is_none(), egui::Button::new(label))
                            .on_hover_text(format!(
                                "{} · {:.1} MiB",
                                package.name,
                                package.size as f64 / 1048576.0
                            ))
                            .clicked()
                        {
                            download = Some(package.clone());
                        }
                    }
                });
            }
            ui.label(&self.updates.status);
            if self.updates.task.is_some() {
                if self.updates.total > 0 {
                    let bytes = self.updates.progress.load(Ordering::Relaxed);
                    ui.add(
                        egui::ProgressBar::new(bytes as f32 / self.updates.total as f32).text(
                            format!(
                                "{:.1} / {:.1} MiB",
                                bytes as f64 / 1048576.0,
                                self.updates.total as f64 / 1048576.0
                            ),
                        ),
                    );
                } else {
                    ui.spinner();
                }
            }
            if !self.updates.error.is_empty() {
                ui.colored_label(ui.visuals().error_fg_color, &self.updates.error);
            }
            if let Some(path) = &self.updates.downloaded {
                ui.label(format!("文件：{}", path.display()));
                if path.to_string_lossy().ends_with("-setup.exe") {
                    if ui.add_enabled(self.updates.task.is_none(), egui::Button::new("安装并重启…")).clicked() {
                        self.updates.confirm_install = true;
                    }
                    if self.updates.confirm_install {
                        ui.label("安装将关闭所有 SSH 和本地终端会话，完成后重新打开 RShell。请先保存当前工作。");
                        ui.horizontal(|ui| {
                            if ui.add_enabled(self.updates.task.is_none(), egui::Button::new("确认安装并重启")).clicked() { install = Some(path.clone()); }
                            if ui.button("暂不安装").clicked() { self.updates.confirm_install = false; }
                        });
                    }
                } else {
                    ui.label("自动安装请下载 Windows 安装包；便携 ZIP 可自行解压使用。");
                }
                if ui
                    .add_enabled(
                        self.updates.task.is_none(),
                        egui::Button::new("打开所在目录"),
                    )
                    .clicked()
                {
                    folder = Some(path.clone());
                }
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(self.updates.task.is_none(), egui::Button::new("重新检查"))
                    .clicked()
                {
                    check = true;
                }
                if self.updates.task.is_some() && ui.button("取消任务").clicked() {
                    if let Some(task) = self.updates.task.take() {
                        task.abort.abort();
                    }
                    self.updates.status = "任务已取消，可重新检查或下载".into();
                }
                if ui.button("关闭").clicked() {
                    self.updates.open = false;
                }
            });
        });
        if check {
            self.check_updates(ctx.clone());
        }
        if let Some(package) = download {
            self.download_update(ctx.clone(), package);
        }
        if let Some(path) = install {
            self.updates.total = 0;
            self.updates.status = "正在准备安装，完成校验后退出并更新…".into();
            self.update_task(ctx.clone(), async move {
                tokio::task::spawn_blocking(move || update::launch_install(path)).await??;
                Ok(Outcome::InstallerStarted)
            });
        }
        if let Some(path) = folder {
            self.updates.total = 0;
            self.update_task(ctx.clone(), async move {
                tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                    anyhow::ensure!(path.is_file(), "下载文件已被移动或删除");
                    std::process::Command::new("explorer.exe")
                        .arg("/select,")
                        .arg(path)
                        .spawn()?;
                    Ok(())
                })
                .await??;
                Ok(Outcome::FolderOpened)
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::App as _;

    #[test]
    fn update_dialog_reports_results_blocks_terminal_and_cancels_work() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/update-ui-tests");
        std::fs::create_dir_all(&root).unwrap();
        let temp = tempfile::tempdir_in(root).unwrap();
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let mut app = App::new(
            &cc,
            Config::default(),
            temp.path().join("config.json"),
            runtime,
            true,
            false,
        )
        .unwrap();
        app.tabs[0].connected = true;
        let render = |app: &mut App, events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800.0, 600.0),
                    )),
                    focused: true,
                    events,
                    ..Default::default()
                },
                |ui| app.ui(ui, &mut eframe::Frame::_new_kittest()),
            );
            output.textures_delta.clear();
            output
        };
        let wait = |app: &mut App| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while app.updates.task.is_some() {
                app.updates.poll();
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
        };
        render(&mut app, vec![]);
        app.updates.open = true;
        app.update_task(ctx.clone(), async {
            Ok(Outcome::Checked(Release {
                version: "999.0.0".into(),
                packages: vec![],
            }))
        });
        wait(&mut app);
        assert_eq!(app.updates.status, "发现新版本");
        render(&mut app, vec![]);
        app.tabs[0].test_sent.clear();
        render(
            &mut app,
            vec![egui::Event::Text("no terminal input".into())],
        );
        assert!(app.tabs[0].test_sent.is_empty());
        app.update_task(ctx.clone(), async { anyhow::bail!("test network failure") });
        wait(&mut app);
        assert!(app.updates.error.contains("test network failure"));
        app.update_task(ctx.clone(), std::future::pending());
        assert!(app.updates.error.is_empty());
        let output = render(&mut app, vec![]);
        let pos = output
            .shapes
            .iter()
            .find_map(|shape| crate::composer_icons::find_text(&shape.shape, "取消任务"))
            .unwrap()
            .visual_bounding_rect()
            .center();
        for pressed in [true, false] {
            render(
                &mut app,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        pressed,
                        button: egui::PointerButton::Primary,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        assert!(app.updates.task.is_none());
        assert!(app.updates.status.contains("已取消"));
        assert!(app.tabs[0].test_sent.is_empty());
        app.updates.open = false;
        app.about_open = true;
        render(&mut app, vec![]);
        let output = render(&mut app, vec![egui::Event::Text("about dialog".into())]);
        for text in [
            "关于 RShell",
            "RShell · 原生 Rust 终端",
            concat!("软件版本：", env!("CARGO_PKG_VERSION")),
        ] {
            assert!(
                output
                    .shapes
                    .iter()
                    .any(|shape| crate::composer_icons::find_text(&shape.shape, text).is_some())
            );
        }
        assert!(app.tabs[0].test_sent.is_empty());
        let close = output
            .shapes
            .iter()
            .find_map(|shape| crate::composer_icons::find_text(&shape.shape, "关闭"))
            .unwrap()
            .visual_bounding_rect()
            .center();
        for pressed in [true, false] {
            render(
                &mut app,
                vec![
                    egui::Event::PointerMoved(close),
                    egui::Event::PointerButton {
                        pos: close,
                        pressed,
                        button: egui::PointerButton::Primary,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        assert!(!app.about_open);
    }
}
