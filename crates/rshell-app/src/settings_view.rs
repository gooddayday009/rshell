use super::*;

fn shortcut_label(binding: &rshell_core::config::KeyBinding) -> String {
    let mut parts = Vec::new();
    if binding.ctrl {
        parts.push("Ctrl");
    }
    if binding.alt {
        parts.push("Alt");
    }
    if binding.shift {
        parts.push("Shift");
    }
    parts.push(match binding.key.as_str() {
        "ArrowLeft" => "←",
        "ArrowRight" => "→",
        "ArrowUp" => "↑",
        "ArrowDown" => "↓",
        "PageUp" => "PageUp",
        "PageDown" => "PageDown",
        other => other,
    });
    parts.join("+")
}

#[cfg(test)]
mod shortcut_tests {
    use super::*;

    #[test]
    fn captures_consumed_key_persists_and_triggers_terminal_search() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/shortcut-tests");
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
        assert_eq!(
            app.config.shortcuts.search_terminal,
            rshell_core::config::KeyBinding::new("F", true, false, true)
        );
        app.shortcut_capture = Some(7);
        let key = || egui::Event::Key {
            key: egui::Key::A,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::CTRL,
        };
        let mut output = ctx.run_ui(
            egui::RawInput {
                events: vec![key()],
                ..Default::default()
            },
            |ui| {
                // Reproduce a focused widget handling the shortcut before the picker.
                assert!(ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::A)));
                app.draw_shortcut_settings(ui);
            },
        );
        output.textures_delta.clear();
        assert!(app.shortcut_capture.is_none());
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.config.shortcuts.search_terminal.key != "A" {
            app.poll_store(&ctx);
            assert!(Instant::now() < deadline, "{}", app.message);
            std::thread::sleep(Duration::from_millis(5));
        }
        let config = Config::load(&app.path).unwrap();
        assert_eq!(
            config.shortcuts.search_terminal,
            rshell_core::config::KeyBinding::new("A", true, false, false)
        );
        let tab = &mut app.tabs[0];
        tab.connected = true;
        tab.focus = true;
        for events in [vec![], vec![key()]] {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800.0, 600.0),
                    )),
                    events,
                    focused: true,
                    ..Default::default()
                },
                |ui| {
                    crate::terminal_view::show(ui, tab, &config, true);
                },
            );
            output.textures_delta.clear();
        }
        assert!(tab.show_search);
        assert!(
            !tab.test_sent
                .iter()
                .any(|command| matches!(command, ssh::Command::Input(_)))
        );
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SettingsPage {
    Appearance,
    Shortcuts,
    Session,
    Connection,
    Ai,
}

pub(super) fn dialog_style(ui: &mut egui::Ui, font_size: f32) {
    let style = ui.style_mut();
    style.spacing.item_spacing = egui::vec2(12.0, 10.0);
    style.spacing.button_padding = egui::vec2(12.0, 7.0);
    style.spacing.interact_size.y = 34.0;
    style.text_styles.insert(
        egui::TextStyle::Body,
        egui::FontId::proportional(font_size + 1.0),
    );
    style.text_styles.insert(
        egui::TextStyle::Button,
        egui::FontId::proportional(font_size + 1.0),
    );
}

impl App {
    pub(super) fn draw_settings(&mut self, ctx: &egui::Context) {
        if !self.settings {
            return;
        }
        let mut open = true;
        let content_height = (ctx.content_rect().height() - 90.0).clamp(300.0, 520.0);
        egui::Window::new("设置")
            .id(egui::Id::unique("app_settings"))
            .open(&mut open)
            .collapsible(false)
            .frame(egui::Frame::window(&ctx.style_of(if self.config.light_theme { egui::Theme::Light } else { egui::Theme::Dark })).inner_margin(20.0))
            .default_size([880.0, 580.0])
            .max_height(ctx.content_rect().height() - 20.0)
            .min_width(680.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                dialog_style(ui, self.config.ui_font_size);
                ui.horizontal_top(|ui| {
                    ui.allocate_ui_with_layout(egui::vec2(170.0, content_height), egui::Layout::top_down(egui::Align::Min), |ui| {
                        ui.heading("设置");
                        ui.add_space(18.0);
                        for (page, title) in [(SettingsPage::Appearance, "外观与终端"), (SettingsPage::Shortcuts, "快捷键"), (SettingsPage::Session, "会话"), (SettingsPage::Connection, "连接 · 保持活动"), (SettingsPage::Ai, "AI")] {
                            ui.add_space(6.0);
                            let selected = self.settings_page == page;
                            let primary = crate::ui_appearance::colors(&self.config, !self.config.light_theme).primary;
                            let button = egui::Button::new(RichText::new(title).color(if selected { crate::ui_appearance::text_on(primary) } else { ui.visuals().text_color() })).fill(if selected { crate::ui_appearance::color(primary) } else { Color32::TRANSPARENT });
                            if ui.add_sized([170.0, 36.0], button).clicked() { self.settings_page = page; }
                        }
                    });
                    let (divider, _) = ui.allocate_exact_size(egui::vec2(1.0, content_height), egui::Sense::hover());
                    ui.painter().vline(divider.center().x, divider.y_range(), ui.visuals().widgets.noninteractive.bg_stroke);
                    ui.add_space(10.0);
                    ui.vertical(|ui| {
                    egui::ScrollArea::vertical().id_salt("settings_content").max_height(content_height).show(ui, |ui| {
                        ui.set_min_width(480.0);
                        match self.settings_page {
                            SettingsPage::Appearance => {
                                ui.heading("外观与终端");
                                ui.weak("调整当前窗口的界面外观与全部终端标签的显示方式。修改后自动保存。");
                                ui.add_space(12.0);
                    ui.strong("界面显示");
                    ui.label("配色主题");
                    let palette_name = crate::ui_appearance::PALETTES.iter().find(|(palette, _, _)| *palette == self.config.ui_palette).unwrap().1;
                    egui::ComboBox::from_id_salt("ui_palette").selected_text(palette_name).width(250.0).show_ui(ui, |ui| {
                        for (palette, label, preview) in crate::ui_appearance::PALETTES {
                            ui.horizontal(|ui| {
                                ui.colored_label(crate::ui_appearance::color(*preview), "●");
                                if ui.selectable_label(self.config.ui_palette == *palette, *label).clicked() {
                                    self.submit(Action::UiAppearance { font: None, font_size: None, zoom: None, palette: Some(*palette), custom_colors: None }, Pending::Update);
                                }
                            });
                        }
                    });
                    if self.config.ui_palette == rshell_core::config::UiPalette::Custom {
                        let mut colors = self.config.custom_ui_colors.clone();
                        let mut changed = false;
                        for (label, value) in [
                            ("背景", &mut colors.background),
                            ("文字", &mut colors.foreground),
                            ("强调色", &mut colors.primary),
                            ("边框", &mut colors.border),
                            ("侧栏", &mut colors.sidebar),
                        ] {
                            ui.horizontal(|ui| {
                                ui.label(label);
                                changed |= ui.color_edit_button_srgb(value).changed();
                            });
                        }
                        if changed {
                            self.submit(Action::UiAppearance { font: None, font_size: None, zoom: None, palette: None, custom_colors: Some(colors) }, Pending::Update);
                        }
                    }
                    ui.horizontal(|ui| {
                        ui.label("界面字体");
                        egui::ComboBox::from_id_salt("ui_font")
                            .selected_text(match self.config.ui_font {
                                rshell_core::config::UiFont::Geist => "Geist",
                                rshell_core::config::UiFont::System => "系统中文字体",
                            })
                            .show_ui(ui, |ui| {
                                for (font, label) in [
                                    (rshell_core::config::UiFont::Geist, "Geist"),
                                    (rshell_core::config::UiFont::System, "系统中文字体"),
                                ] {
                                    if ui.selectable_label(self.config.ui_font == font, label).clicked() {
                                        self.submit(Action::UiAppearance { font: Some(font), font_size: None, zoom: None, palette: None, custom_colors: None }, Pending::Update);
                                    }
                                }
                            });
                    });
                    ui.label("界面字号");
                    let mut ui_size = self.config.ui_font_size;
                    if ui.add(egui::Slider::new(&mut ui_size, 10.0..=22.0)).changed() {
                        self.submit(Action::UiAppearance { font: None, font_size: Some(ui_size), zoom: None, palette: None, custom_colors: None }, Pending::Update);
                    }
                    ui.label("界面缩放");
                    let mut zoom = (self.config.ui_zoom * 100.0).round();
                    if ui.add(egui::Slider::new(&mut zoom, 75.0..=150.0).suffix("%")).changed() {
                        self.submit(Action::UiAppearance { font: None, font_size: None, zoom: Some(zoom / 100.0), palette: None, custom_colors: None }, Pending::Update);
                    }
                    ui.weak("缩放作用于整个窗口，也会放大终端；终端字号可在下方单独调整。系统字体不可用时回退 Geist。");
                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(10.0);
                    ui.strong("终端显示");
                    ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                    ui.label("字体");
                    let font_name = crate::terminal_appearance::FONTS.iter().find(|(font, _, _)| *font == self.config.terminal_font).unwrap().1;
                    egui::ComboBox::from_id_salt("terminal_font").selected_text(font_name).width(250.0).show_ui(ui, |ui| {
                        for (font, label, _) in crate::terminal_appearance::FONTS {
                            let available = crate::terminal_appearance::font_available(*font);
                            if ui.add_enabled(available, egui::Button::selectable(self.config.terminal_font == *font, if available { (*label).to_owned() } else { format!("{label}（未安装）") })).clicked() {
                                self.submit(Action::Preferences { font_size: None, terminal_font: Some(*font), terminal_theme: None, light_theme: None, copy_on_select: None, trim_paste: None }, Pending::Update);
                            }
                        }
                    });
                    });
                    ui.add_space(12.0);
                    ui.vertical(|ui| {
                    ui.label("主题");
                    let theme_name = crate::terminal_appearance::THEMES.iter().find(|(theme, _)| *theme == self.config.terminal_theme).unwrap().1;
                    egui::ComboBox::from_id_salt("terminal_theme").selected_text(theme_name).width(170.0).show_ui(ui, |ui| {
                        for (theme, label) in crate::terminal_appearance::THEMES {
                            if ui.selectable_label(self.config.terminal_theme == *theme, *label).clicked() {
                                self.submit(Action::Preferences { font_size: None, terminal_font: None, terminal_theme: Some(*theme), light_theme: None, copy_on_select: None, trim_paste: None }, Pending::Update);
                            }
                        }
                    });
                    });
                    });
                    ui.weak("未安装的字体不可选；已有配置在字体缺失时回退 Geist Mono。中文使用系统字体回退。");
                    ui.label("终端字号");
                    let mut size = self.config.font_size;
                    if ui.add(egui::Slider::new(&mut size, 10.0..=32.0)).changed() {
                        self.submit(
                            Action::Preferences {
                                font_size: Some(size),
                                terminal_font: None,
                                terminal_theme: None,
                                light_theme: None,
                                copy_on_select: None,
                                trim_paste: None,
                            },
                            Pending::Update,
                        );
                    }
                    ui.add_space(14.0);
                    ui.separator();
                    ui.add_space(10.0);
                    ui.strong("配色主题");
                    ui.horizontal(|ui| {
                        for (label, light) in [("深色界面", false), ("浅色界面", true)] {
                            if ui.add(egui::Button::selectable(self.config.light_theme == light, label)).clicked() {
                                self.submit(
                                    Action::Preferences {
                                        font_size: None,
                                        terminal_font: None,
                                        terminal_theme: None,
                                        light_theme: Some(light),
                                        copy_on_select: None,
                                        trim_paste: None,
                                    },
                                    Pending::Update,
                                );
                            }
                        }
                    });
                    ui.add_space(12.0);
                    ui.strong("终端配色预览");
                    ui.weak("主题影响默认色和 ANSI 16 色；应用界面明暗单独设置，远端真彩色保持原样。");
                    let (background, foreground, ansi) = crate::terminal_appearance::colors(self.config.terminal_theme);
                    egui::Frame::new().fill(egui::Color32::from_rgb(background[0], background[1], background[2])).inner_margin(10.0).show(ui, |ui| {
                        ui.label(egui::RichText::new("RShell  ~/project  $ ls -la").font(crate::terminal_appearance::font_id(self.config.terminal_font, self.config.font_size)).color(egui::Color32::from_rgb(foreground[0], foreground[1], foreground[2])));
                        ui.horizontal_wrapped(|ui| {
                            for (index, name) in ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"].iter().enumerate() {
                                let rgb = ansi[index];
                                let mut text = egui::RichText::new(*name).monospace().color(egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
                                if index == 0 { text = text.background_color(egui::Color32::from_gray(100)); }
                                ui.label(text);
                            }
                        });
                        ui.horizontal_wrapped(|ui| {
                            for (index, name) in ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"].iter().enumerate() {
                                let rgb = ansi[index + 8];
                                ui.label(egui::RichText::new(*name).monospace().color(egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2])));
                            }
                        });
                    });
                    ui.add_space(14.0);
                    ui.separator();
                    ui.add_space(10.0);
                    ui.strong("复制与粘贴");
                    let mut copy = self.config.copy_on_select;
                    if ui.checkbox(&mut copy, "选定文本后自动复制到剪贴板").changed() {
                        self.submit(Action::Preferences { font_size: None, terminal_font: None, terminal_theme: None, light_theme: None, copy_on_select: Some(copy), trim_paste: None }, Pending::Update);
                    }
                    let mut trim = self.config.trim_paste;
                    if ui.checkbox(&mut trim, "粘贴时自动删除尾部换行和空白").changed() {
                        self.submit(Action::Preferences { font_size: None, terminal_font: None, terminal_theme: None, light_theme: None, copy_on_select: None, trim_paste: Some(trim) }, Pending::Update);
                    }
                    ui.label("清理后仍包含多行的内容，需要确认后粘贴。");
                    ui.separator();
                    ui.strong("配置与使用说明");
                    ui.label("配置文件");
                    ui.add(egui::Label::new(self.path.display().to_string()).wrap());
                    ui.horizontal(|ui| {
                        ui.label("终端回看上限");
                        if self.smoke && std::env::args().any(|a| a == "--smoke-history-settings") { ui.scroll_to_cursor(Some(egui::Align::Center)); }
                        let draft = self.scrollback_draft.get_or_insert(self.config.scrollback_lines);
                        ui.add(egui::DragValue::new(draft).range(0..=rshell_core::config::MAX_SCROLLBACK_LINES).speed(100).suffix(" 行"));
                        let lines = *draft;
                        if ui.add_enabled(lines != self.config.scrollback_lines, egui::Button::new("应用")).clicked() {
                            self.submit(Action::ScrollbackLines(lines), Pending::Update);
                        }
                    });
                    ui.weak("范围 0–100000 行，0 表示不保留回看。应用到全部终端并保存；降低上限会丢弃超出的旧历史。行数越多，内存占用越大。");
                    ui.label("Shift + 鼠标可强制选择文本。");
                    ui.separator();

                            }
                            SettingsPage::Connection => self.draw_connection_settings(ui),
                            SettingsPage::Shortcuts => self.draw_shortcut_settings(ui),
                            SettingsPage::Session => self.draw_session_settings(ui),
                            SettingsPage::Ai => self.draw_ai_settings(ui),
                        }
                    });
                    });
                });
            });
        self.settings = open;
        if !open {
            self.shortcut_capture = None;
            self.scrollback_draft = None;
        }
    }

    fn draw_shortcut_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("快捷键");
        ui.weak("点击键位后按新组合键。需要 Ctrl 或 Alt；Esc 取消录入。修改后自动保存。");
        ui.add_space(12.0);
        let names = [
            "新建本地 PowerShell",
            "关闭当前标签",
            "上一个标签",
            "下一个标签",
            "上一个标签（备用）",
            "下一个标签（备用）",
            "复制终端选区",
            "查找终端内容",
        ];
        let shortcuts = self.config.shortcuts.clone();
        let bindings = shortcuts.bindings();
        for (index, name) in names.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.add_sized([220.0, 32.0], egui::Label::new(*name));
                let binding = bindings[index];
                let label = if self.shortcut_capture == Some(index) {
                    "请按快捷键…".into()
                } else {
                    shortcut_label(binding)
                };
                if ui
                    .add_sized([210.0, 32.0], egui::Button::new(label))
                    .clicked()
                {
                    self.shortcut_capture = Some(index);
                }
            });
        }
        ui.add_space(10.0);
        if ui.button("恢复默认快捷键").clicked() {
            self.shortcut_capture = None;
            self.submit(Action::Shortcuts(Default::default()), Pending::Update);
        }
        if let Some(index) = self.shortcut_capture
            && ui.ctx().current_pass_index() == 0
        {
            let pressed = ui.input(|input| {
                // Widgets may consume Ctrl+A, arrow keys and other shortcuts before
                // the picker is drawn. Capture the original event, not the leftovers.
                input.raw.events.iter().find_map(|event| match event {
                    egui::Event::Key {
                        key,
                        pressed: true,
                        repeat: false,
                        modifiers,
                        ..
                    } => Some((*key, *modifiers)),
                    egui::Event::Copy => Some((egui::Key::C, input.modifiers)),
                    egui::Event::Cut => Some((egui::Key::X, input.modifiers)),
                    egui::Event::Paste(_) => Some((egui::Key::V, input.modifiers)),
                    _ => None,
                })
            });
            if let Some((key, modifiers)) = pressed {
                if key == egui::Key::Escape {
                    self.shortcut_capture = None;
                } else if modifiers.ctrl || modifiers.alt {
                    let mut shortcuts = self.config.shortcuts.clone();
                    *shortcuts.binding_mut(index) = rshell_core::config::KeyBinding::new(
                        &format!("{key:?}"),
                        modifiers.ctrl,
                        modifiers.alt,
                        modifiers.shift,
                    );
                    if modifiers.ctrl
                        && !modifiers.shift
                        && matches!(key, egui::Key::C | egui::Key::V | egui::Key::X)
                    {
                        self.message = "Ctrl+C、Ctrl+V、Ctrl+X 已由终端输入和剪贴板保留".into();
                    } else if shortcuts.valid() {
                        if self.submit(Action::Shortcuts(shortcuts), Pending::Update) {
                            self.shortcut_capture = None;
                        }
                    } else {
                        self.message = "快捷键重复，或使用了 Enter、Tab 等保留按键".into();
                    }
                }
            }
        }
    }

    fn draw_session_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("会话");
        ui.add_space(12.0);
        ui.add_enabled_ui(false, |ui| {
            let mut enabled = true;
            ui.checkbox(&mut enabled, "在单一 RShell 窗口中打开多个会话");
        });
        ui.weak("当前版本通过同一窗口的标签页管理多个会话。");
        let mut options = self.config.session_options.clone();
        ui.checkbox(&mut options.confirm_exit, "退出 RShell 时确认断开连接");
        ui.checkbox(&mut options.confirm_disconnect, "会话终止时确认断开连接");
        ui.checkbox(
            &mut options.restore_local_shell,
            "连接终止时还原为本地 Shell",
        );
        ui.checkbox(
            &mut options.clear_scrollback_on_close,
            "清除滚动缓冲区，当连接关闭",
        );
        ui.checkbox(
            &mut options.auto_save_session,
            "自动将更改保存到当前会话文件",
        );
        ui.weak("自动保存适用于已保存会话的“连接”操作；凭据变更仍需显式保存。");
        ui.checkbox(
            &mut options.remember_secret_by_default,
            "新建 SSH 会话默认记住密码 / 口令",
        );
        ui.weak("只影响之后新建的 SSH 会话；保存会话时仍可单独取消勾选。");
        use rshell_core::store::SessionOption;
        for (field, before, after) in [
            (
                SessionOption::ConfirmExit,
                self.config.session_options.confirm_exit,
                options.confirm_exit,
            ),
            (
                SessionOption::ConfirmDisconnect,
                self.config.session_options.confirm_disconnect,
                options.confirm_disconnect,
            ),
            (
                SessionOption::RestoreLocalShell,
                self.config.session_options.restore_local_shell,
                options.restore_local_shell,
            ),
            (
                SessionOption::ClearScrollbackOnClose,
                self.config.session_options.clear_scrollback_on_close,
                options.clear_scrollback_on_close,
            ),
            (
                SessionOption::AutoSaveSession,
                self.config.session_options.auto_save_session,
                options.auto_save_session,
            ),
            (
                SessionOption::RememberSecretByDefault,
                self.config.session_options.remember_secret_by_default,
                options.remember_secret_by_default,
            ),
        ] {
            if before != after {
                self.submit(Action::SessionOption(field, after), Pending::Update);
            }
        }
    }

    fn draw_connection_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("保持活动状态");
        ui.label("按 SSH 会话保存，下次连接生效。");
        ui.add_space(12.0);
        let profiles: Vec<_> = self
            .config
            .profiles
            .iter()
            .filter(|p| p.protocol == Protocol::Ssh)
            .cloned()
            .collect();
        if profiles.is_empty() {
            ui.label("尚无 SSH 会话，请先在连接窗口保存会话。");
            return;
        }
        if !profiles.iter().any(|p| {
            self.keep_alive_draft
                .as_ref()
                .is_some_and(|(id, _)| *id == p.id)
        }) {
            self.keep_alive_draft = Some((profiles[0].id, profiles[0].keep_alive.clone()));
        }
        let (id, settings) = self.keep_alive_draft.as_mut().unwrap();
        let previous = *id;
        egui::ComboBox::from_id_salt("keep_alive_profile")
            .selected_text(profiles.iter().find(|p| p.id == *id).unwrap().name.clone())
            .show_ui(ui, |ui| {
                for profile in &profiles {
                    ui.selectable_value(
                        id,
                        profile.id,
                        format!("{} · {}", profile.name, profile.endpoint()),
                    );
                }
            });
        if *id != previous {
            *settings = profiles
                .iter()
                .find(|p| p.id == *id)
                .unwrap()
                .keep_alive
                .clone();
        }
        ui.weak("切换会话会放弃尚未保存的修改。");
        ui.add_space(16.0);
        ui.checkbox(
            &mut settings.ssh_enabled,
            "连接会话期间发送保持活动状态消息",
        );
        ui.add_enabled_ui(settings.ssh_enabled, |ui| {
            ui.horizontal(|ui| {
                ui.label("间隔：");
                ui.add(
                    egui::DragValue::new(&mut settings.ssh_interval_secs)
                        .range(1..=86400)
                        .suffix(" 秒"),
                );
            });
        });
        ui.checkbox(&mut settings.idle_enabled, "网络空闲时发送字符串");
        ui.add_enabled_ui(settings.idle_enabled, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("间隔：");
                            ui.add(egui::DragValue::new(&mut settings.idle_interval_secs).range(1..=86400).suffix(" 秒"));
                        });
                        ui.horizontal(|ui| {
                            ui.label("字符串：");
                            ui.add(egui::TextEdit::singleline(&mut settings.idle_text).char_limit(1024).desired_width(240.0));
                        });
                        ui.label("按 UTF-8 原样发送，不追加回车、不解析转义；内容会进入远端终端并明文保存，请勿填写秘密。");
                    });
        ui.checkbox(
            &mut settings.tcp_enabled,
            "网络为空闲状态时发送 TCP 保持活动数据包",
        );
        ui.small(
            "TCP 探测使用系统默认时间。保存后下次连接生效；不影响本地 PowerShell 中的 ssh 命令。",
        );

        ui.add_space(16.0);
        let save = ui.button("保存保活设置").clicked();
        let action = Action::KeepAlive {
            id: *id,
            settings: settings.clone(),
        };
        if save {
            self.submit(action, Pending::Update);
        }
    }
}

impl App {
    pub(super) fn draw_host_keys(&mut self, ui: &mut egui::Ui) {
        ui.heading("已信任的 SSH 主机");
        ui.label("仅管理内置 SSH 会话；命令行 ssh 的信任记录由系统 OpenSSH 管理。");
        egui::ScrollArea::vertical()
            .max_height(260.0)
            .show(ui, |ui| {
                for host in self.config.known_hosts.clone() {
                    ui.group(|ui| {
                        ui.strong(format!("{}:{}", host.host, host.port));
                        ui.monospace(&host.fingerprint);
                        ui.label(format!(
                            "算法：{}",
                            if host.algorithm.is_empty() {
                                "旧记录未保存"
                            } else {
                                &host.algorithm
                            }
                        ));
                        ui.label(match host.confirmed_at {
                            Some(timestamp) => {
                                format!("记录时间（Unix 秒 / UTC）：{timestamp}")
                            }
                            None => "记录时间：旧记录未知".into(),
                        });
                        if ui.button("核验 / 更新 / 移除…").clicked() {
                            self.host_editor = Some(HostEditor {
                                original: host.clone(),
                                public_key: host.public_key,
                                fingerprint: String::new(),
                                verified: false,
                                error: String::new(),
                            });
                        }
                    });
                }
                if self.config.known_hosts.is_empty() {
                    ui.label("尚无已信任的主机。");
                }
            });
    }
}
