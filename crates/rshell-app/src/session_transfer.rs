use super::*;

pub(super) struct Dialog {
    path: PathBuf,
    export: bool,
    include_passwords: bool,
    password: Zeroizing<String>,
    confirmation: Zeroizing<String>,
}
impl Dialog {
    pub(super) fn smoke_export() -> Self {
        Self {
            include_passwords: true,
            ..Self::export(PathBuf::from("sessions.rsh"))
        }
    }
    pub(super) fn export(path: PathBuf) -> Self {
        Self {
            path,
            export: true,
            include_passwords: false,
            password: Default::default(),
            confirmation: Default::default(),
        }
    }
    pub(super) fn import(path: PathBuf) -> Self {
        Self {
            export: false,
            include_passwords: true,
            ..Self::export(path)
        }
    }
}
impl App {
    pub(super) fn session_transfer_dialog(&mut self, ctx: &egui::Context) {
        let busy = self
            .pending
            .values()
            .any(|p| matches!(p, Pending::SessionFile));
        let Some(dialog) = self.transfer_dialog.as_mut() else {
            return;
        };
        let mut open = true;
        let mut submit = false;
        let mut cancel = false;
        egui::Window::new(if dialog.export {
            "导出 RShell 会话"
        } else {
            "导入加密 RShell 会话"
        })
        .open(&mut open)
        .collapsible(false)
        .default_width(460.0)
        .vscroll(true)
        .max_height((ctx.content_rect().height() - 80.0).max(240.0))
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.label(dialog.path.display().to_string());
            ui.add_enabled_ui(!busy, |ui| {
                if dialog.export {
                    let count = self
                        .config
                        .profiles
                        .iter()
                        .filter(|p| {
                            p.remember_secret
                                && p.protocol == Protocol::Ssh
                                && p.auth == AuthMethod::Password
                        })
                        .count();
                    ui.label(format!(
                        "全部 {} 个会话；其中 {} 个配置为保存登录密码。",
                        self.config.profiles.len(),
                        count
                    ));
                    ui.checkbox(
                        &mut dialog.include_passwords,
                        "包含已保存的会话密码（加密整个文件）",
                    );
                    ui.weak("不包含私钥、私钥口令、API Key 或 JumpServer 临时凭证。");
                }
                if dialog.include_passwords {
                    ui.label(if dialog.export {
                        "设置导出口令"
                    } else {
                        "请输入此备份的导出口令"
                    });
                    ui.add(
                        egui::TextEdit::singleline(&mut *dialog.password)
                            .password(true)
                            .char_limit(16384),
                    );
                    if dialog.export {
                        ui.label("确认口令");
                        ui.add(
                            egui::TextEdit::singleline(&mut *dialog.confirmation)
                                .password(true)
                                .char_limit(16384),
                        );
                        ui.weak("口令不保存；遗忘后无法恢复备份。建议使用较长且独立的口令。");
                    }
                } else {
                    dialog.password = Default::default();
                    dialog.confirmation = Default::default();
                }
                let valid = !dialog.include_passwords
                    || (!dialog.password.is_empty()
                        && (!dialog.export || dialog.password == dialog.confirmation));
                ui.horizontal(|ui| {
                    submit = ui
                        .add_enabled(
                            valid,
                            egui::Button::new(if dialog.export {
                                "导出"
                            } else {
                                "解密并导入"
                            }),
                        )
                        .clicked();
                    cancel = ui.button("取消").clicked();
                });
            });
            if busy {
                ui.spinner();
                ui.label("正在后台处理，请稍候…");
            }
            if !self.message.is_empty() {
                ui.separator();
                ui.label(&self.message);
            }
        });
        if submit {
            let password = std::mem::take(&mut dialog.password);
            dialog.confirmation = Default::default();
            let path = dialog.path.clone();
            let action = if dialog.export {
                if dialog.include_passwords {
                    Action::ExportEncryptedRsh { path, password }
                } else {
                    Action::ExportRsh { path }
                }
            } else {
                Action::ImportEncryptedRsh { path, password }
            };
            self.message.clear();
            self.submit(action, Pending::SessionFile);
        } else if !busy && (!open || cancel) {
            self.transfer_dialog = None;
        }
    }
}
