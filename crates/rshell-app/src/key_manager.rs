use super::*;
use rshell_core::user_keys::{self, KeyInfo};

type KeyResult = Result<Vec<KeyInfo>, String>;

#[derive(Default)]
pub(super) struct KeyManager {
    pub open: bool,
    pub hosts_open: bool,
    items: Vec<KeyInfo>,
    selected: Option<PathBuf>,
    task: Option<std::sync::mpsc::Receiver<KeyResult>>,
    name: String,
    rsa: bool,
    bits: usize,
    password: Zeroizing<String>,
    notice: String,
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
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO).default_size([780.0, 560.0]).show(ctx, |ui| {
            settings_view::dialog_style(ui, self.config.ui_font_size);
            ui.weak("私钥由当前系统用户加密保护；不会显示私钥正文。名称也作为公钥注释。");
            egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                egui::Grid::new("user_key_list").striped(true).min_col_width(130.0).show(ui, |ui| {
                    ui.strong("名称 / 注释"); ui.strong("类型"); ui.strong("长度（位）"); ui.end_row();
                    for item in &self.keys.items {
                        if ui.selectable_label(self.keys.selected.as_ref() == Some(&item.path), &item.name).clicked() {
                            self.keys.selected = Some(item.path.clone());
                            self.keys.delete = false;
                            self.keys.properties = false;
                        }
                        ui.label(&item.algorithm);
                        ui.label(if item.bits == 0 { "—".into() } else { item.bits.to_string() });
                        ui.end_row();
                    }
                });
                if self.keys.items.is_empty() { ui.label("暂无用户密钥。填写名称后生成，或导入已有私钥。"); }
            });
            let selected = self.keys.items.iter().find(|k| Some(&k.path) == self.keys.selected.as_ref()).cloned();
            let idle = self.keys.task.is_none();
            ui.add_enabled_ui(idle, |ui| {
                ui.horizontal(|ui| {
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
                ui.separator();
                ui.horizontal(|ui| { ui.label("名称 / 注释"); ui.text_edit_singleline(&mut self.keys.name); });
                ui.horizontal(|ui| {
                    ui.label("生成类型");
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
                ui.horizontal(|ui| {
                    ui.label("导入 / 导出口令");
                    ui.add(egui::TextEdit::singleline(&mut *self.keys.password).password(true));
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
