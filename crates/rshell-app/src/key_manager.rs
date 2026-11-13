use super::*;
use rshell_core::user_keys::{self, KeyInfo};

type KeyResult = Result<Vec<KeyInfo>, String>;

#[derive(Default)]
pub(super) struct KeyManager {
    pub open: bool,
    pub hosts_open: bool,
    pub(super) items: Vec<KeyInfo>,
    pub(super) loaded: bool,
    pub(super) refresh_requested: bool,
    selected: Option<PathBuf>,
    pub(super) task: Option<std::sync::mpsc::Receiver<KeyResult>>,
    name: String,
    rsa: bool,
    bits: usize,
    password: Zeroizing<String>,
    pub(super) notice: String,
    delete: bool,
    properties: bool,
}

impl App {
    fn key_task(
        &mut self,
        ctx: egui::Context,
        action: impl FnOnce(PathBuf) -> anyhow::Result<()> + Send + 'static,
    ) {
        if self.keys.task.is_some() {
            return;
        }
        let directory = self.path.parent().unwrap().join("user-keys");
        let (tx, rx) = std::sync::mpsc::channel();
        self.keys.task = Some(rx);
        self.keys.loaded = true;
        self.keys.refresh_requested = false;
        self.keys.notice = "正在处理…".into();
        std::thread::spawn(move || {
            let result = action(directory.clone())
                .and_then(|()| user_keys::list(&directory))
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    pub(super) fn refresh_user_keys(&mut self, ctx: egui::Context) {
        self.key_task(ctx, |_| Ok(()));
    }

    pub(super) fn draw_key_managers(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.keys.task {
            match rx.try_recv() {
                Ok(result) => {
                    self.keys.task = None;
                    match result {
                        Ok(items) => {
                            if !items
                                .iter()
                                .any(|item| Some(&item.path) == self.keys.selected.as_ref())
                            {
                                self.keys.selected = None;
                                self.keys.properties = false;
                                self.keys.delete = false;
                            }
                            self.keys.items = items;
                            self.keys.notice = "操作完成".into();
                        }
                        Err(error) => self.keys.notice = error,
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.keys.task = None;
                    self.keys.notice = "后台密钥操作中断，请刷新检查结果".into();
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.smoke && std::env::args().any(|a| a == "--smoke-user-keys") {
            self.keys.open = true;
        }
        if self.smoke && std::env::args().any(|a| a == "--smoke-host-keys") {
            self.keys.hosts_open = true;
        }
        if self.keys.hosts_open {
            let mut open = true;
            egui::Window::new("主机密钥管理器")
                .open(&mut open)
                .collapsible(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .default_size([680.0, 440.0])
                .show(ctx, |ui| {
                    settings_view::dialog_style(ui, self.config.ui_font_size);
                    self.draw_host_keys(ui);
                });
            self.keys.hosts_open = open;
        }
        if !self.keys.open {
            return;
        }
        let mut open = true;
        egui::Window::new("用户密钥管理器").open(&mut open)
                .collapsible(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .frame(egui::Frame::window(&ctx.style_of(if self.config.light_theme { egui::Theme::Light } else { egui::Theme::Dark })).inner_margin(16.0))
                .default_size([640.0, 480.0]).min_width(480.0).vscroll(true).show(ctx, |ui| {
            settings_view::dialog_style(ui, self.config.ui_font_size);
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            ui.weak("私钥由当前系统用户加密保护；不会显示私钥正文。名称也作为公钥注释。");
            ui.add_space(12.0);
            egui_extras::TableBuilder::new(ui)
                .id_salt("user_key_list")
                .striped(true)
                .sense(egui::Sense::click())
                .column(egui_extras::Column::remainder().at_least(120.0))
                .column(egui_extras::Column::initial(112.0))
                .column(egui_extras::Column::initial(88.0))
                .max_scroll_height(200.0)
                .header(32.0, |mut row| {
                    row.col(|ui| { ui.strong("名称 / 注释"); });
                    row.col(|ui| { ui.strong("类型"); });
                    row.col(|ui| { ui.strong("长度（位）"); });
                })
                .body(|mut body| {
                    for item in &self.keys.items {
                        body.row(36.0, |mut row| {
                            row.set_selected(self.keys.selected.as_ref() == Some(&item.path));
                            row.col(|ui| { ui.add(egui::Label::new(&item.name).truncate().selectable(false)); });
                            row.col(|ui| { ui.add(egui::Label::new(&item.algorithm).selectable(false)); });
                            row.col(|ui| { ui.add(egui::Label::new(RichText::new(if item.bits == 0 { "—".into() } else { item.bits.to_string() }).monospace()).selectable(false)); });
                            if row.response().on_hover_text(&item.fingerprint).clicked() {
                                self.keys.selected = Some(item.path.clone());
                                self.keys.delete = false;
                                self.keys.properties = false;
                            }
                        });
                    }
                });
            if self.keys.items.is_empty() { ui.weak("暂无用户密钥，填写名称后生成或导入已有私钥"); }
            ui.add_space(8.0);
            let selected = self.keys.items.iter().find(|k| Some(&k.path) == self.keys.selected.as_ref()).cloned();
            let idle = self.keys.task.is_none();
            ui.add_enabled_ui(idle, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui.button("刷新").clicked() { self.refresh_user_keys(ctx.clone()); }
                    if ui.add_enabled(selected.is_some(), egui::Button::new("属性")).clicked() { self.keys.properties = !self.keys.properties; }
                    if ui.add_enabled(selected.is_some(), egui::Button::new("删除…")).clicked() { self.keys.delete = true; }
                    if let Some(item) = &selected {
                        if ui.button("复制公钥").clicked() { ui.ctx().copy_text(item.public_key.clone()); }
                        if ui.button("导出公钥…").clicked() {
                            let public = item.public_key.clone();
                            self.key_task(ctx.clone(), move |_| {
                                if let Some(path) = rfd::FileDialog::new().set_file_name("id_rshell.pub").save_file() {
                                    use std::io::Write;
                                    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
                                    writeln!(file, "{public}")?;
                                }
                                Ok(())
                            });
                        }
                    }
                });
                if let Some(item) = &selected {
                    if self.keys.properties {
                        ui.group(|ui| {
                            ui.label(format!("名称 / 注释：{}", item.name));
                            ui.monospace(&item.fingerprint);
                            ui.label(item.path.display().to_string());
                            ui.add(egui::Label::new(&item.public_key).wrap());
                        });
                    }
                    if self.keys.delete {
                        ui.colored_label(Color32::YELLOW, "删除后无法恢复；引用此密钥的会话下次认证会失败。已有连接不受影响。");
                        ui.horizontal(|ui| {
                            if ui.button("确认删除此密钥").clicked() {
                                let path = item.path.clone();
                                self.key_task(ctx.clone(), move |_| { std::fs::remove_file(path)?; Ok(()) });
                                self.keys.delete = false;
                            }
                            if ui.button("取消").clicked() { self.keys.delete = false; }
                        });
                    }
                    ui.menu_button("用于 SSH 会话…", |ui| {
                        for profile in self.config.profiles.clone().into_iter().filter(|p| p.protocol == Protocol::Ssh) {
                            if ui.button(&profile.name).clicked() {
                                let mut editor = Editor::new(profile);
                                editor.profile.auth = AuthMethod::PrivateKey;
                                editor.profile.key_path = item.path.to_string_lossy().into_owned();
                                editor.profile.remember_secret = false;
                                self.editor = Some(editor);
                                self.keys.open = false;
                                ui.close();
                            }
                        }
                        ui.weak("选择后在会话属性中检查并保存；托管密钥无需填写口令。");
                    });
                }
                ui.add_space(8.0);
                ui.separator();
                ui.label("生成或导入密钥");
                egui::Grid::new("user_key_fields").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                ui.label("名称 / 注释");
                ui.add(egui::TextEdit::singleline(&mut self.keys.name).desired_width(280.0));
                ui.end_row();
                ui.label("生成类型");
                ui.horizontal(|ui| {

                    ui.selectable_value(&mut self.keys.rsa, false, "Ed25519");
                    ui.selectable_value(&mut self.keys.rsa, true, "RSA");
                    if self.keys.rsa {
                        if self.keys.bits == 0 { self.keys.bits = 3072; }
                        egui::ComboBox::from_id_salt("rsa_bits").selected_text(format!("{} 位", self.keys.bits)).show_ui(ui, |ui| {
                            for bits in [2048, 3072, 4096] { ui.selectable_value(&mut self.keys.bits, bits, format!("{bits} 位")); }
                        });
                    }
                    if ui.button("生成").clicked() {
                        let name = self.keys.name.clone();
                        let bits = self.keys.rsa.then_some(self.keys.bits);
                        self.key_task(ctx.clone(), move |dir| { user_keys::save(&dir, &user_keys::generate(&name, bits)?, &name)?; Ok(()) });
                    }
                });
                ui.end_row();
                ui.label("导入 / 导出口令");
                ui.add(egui::TextEdit::singleline(&mut *self.keys.password).password(true).desired_width(280.0));
                ui.end_row();
                });
                ui.weak("导入支持 OpenSSH / PEM 私钥；导出为有口令保护的 OpenSSH 私钥，目标须为新文件。");
                ui.horizontal(|ui| {
                    if ui.button("导入…").clicked() {
                        let name = self.keys.name.clone();
                        let password = std::mem::take(&mut self.keys.password);
                        self.key_task(ctx.clone(), move |dir| {
                            if let Some(path) = rfd::FileDialog::new().set_title("导入 SSH 私钥").pick_file() {
                                let key = user_keys::load(path, if password.is_empty() { None } else { Some(password.as_str()) })?;
                                user_keys::save(&dir, &key, &name)?;
                            }
                            Ok(())
                        });
                    }
                    if ui.add_enabled(selected.is_some() && !self.keys.password.is_empty(), egui::Button::new("导出私钥…")).clicked() {
                        let source = selected.as_ref().unwrap().path.clone();
                        let password = std::mem::take(&mut self.keys.password);
                        self.key_task(ctx.clone(), move |_| {
                            if let Some(path) = rfd::FileDialog::new().set_file_name("id_rshell").save_file() {
                                user_keys::export_private(&source, &path, password.as_str())?;
                            }
                            Ok(())
                        });
                    }
                });
            });
            ui.label(&self.keys.notice);
        });
        self.keys.open &= open;
        if !self.keys.open {
            self.keys.password.zeroize();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(app: &mut App, ctx: &egui::Context, events: Vec<egui::Event>) -> egui::FullOutput {
        let mut output = ctx.run_ui(
            egui::RawInput {
                time: Some(ctx.input(|i| i.time) + 0.3),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    if app.keys.open {
                        egui::vec2(600.0, 760.0)
                    } else {
                        egui::vec2(1200.0, 900.0)
                    },
                )),
                events,
                ..Default::default()
            },
            |_| {
                app.draw_key_managers(ctx);
                app.editor(ctx);
            },
        );
        output.textures_delta.clear();
        output
    }

    fn click(app: &mut App, ctx: &egui::Context, text: &str) {
        render(app, ctx, vec![]);
        let output = render(app, ctx, vec![]);
        let pos = output
            .shapes
            .iter()
            .find_map(|shape| {
                crate::composer_icons::find_text(&shape.shape, text)
                    .map(|s| s.pos + s.galley.size() / 2.0)
            })
            .unwrap_or_else(|| panic!("missing {text}"));
        for pressed in [true, false] {
            render(
                app,
                ctx,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
    }

    #[test]
    fn key_row_and_session_picker_select_identity_without_old_secret() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/key-manager-tests");
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
        let path = temp.path().join("user-keys/test.rkey");
        app.keys.items = vec![KeyInfo {
            path: path.clone(),
            name: "test-key".into(),
            algorithm: "ssh-ed25519".into(),
            bits: 256,
            fingerprint: "SHA256:test".into(),
            public_key: String::new(),
        }];
        app.keys.loaded = true;
        app.keys.open = true;
        // Clicking the algorithm cell must select the entire key, not only its name.
        click(&mut app, &ctx, "ssh-ed25519");
        assert_eq!(app.keys.selected, Some(path.clone()));
        app.keys.open = false;
        let mut editor = Editor::new(Profile {
            auth: AuthMethod::PrivateKey,
            remember_secret: true,
            ..Default::default()
        });
        editor.secret = "old-password".into();
        editor.loaded_identity = Some(editor.identity());
        app.editor = Some(editor);
        click(&mut app, &ctx, "选择已生成 / 导入的密钥");
        click(&mut app, &ctx, "test-key · ssh-ed25519");
        let editor = app.editor.as_ref().unwrap();
        assert_eq!(editor.profile.key_path, path.to_string_lossy());
        assert!(editor.secret.is_empty());
        assert!(editor.loaded_identity.is_none());
        assert!(!editor.profile.remember_secret);
    }
}
