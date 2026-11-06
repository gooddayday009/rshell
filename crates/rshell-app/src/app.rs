#[path = "split_view.rs"]
mod split_view;
use split_view::{PaneGroups, terminal_panes};
#[path = "agent_view.rs"]
mod agent_view;
#[path = "ai_settings.rs"]
mod ai_settings;
#[path = "jumpserver_view.rs"]
mod jumpserver_view;
#[path = "key_manager.rs"]
mod key_manager;
#[path = "settings_view.rs"]
mod settings_view;
#[path = "transfer_view.rs"]
mod transfer_view;
#[path = "update_view.rs"]
mod update_view;
use eframe::egui::{self, Color32, RichText};
use rshell_core::{
    config::{
        AuthMethod, Config, Folder, KnownHost, Profile, Protocol, SerialFlowControl, SerialParity,
    },
    ssh,
    store::{Action, Outcome, Store},
};
use rshell_terminal::Terminal;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::oneshot;
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

pub(crate) fn shortcut_matches(
    binding: &rshell_core::config::KeyBinding,
    key: egui::Key,
    modifiers: egui::Modifiers,
) -> bool {
    binding.key == format!("{key:?}")
        && binding.ctrl == modifiers.ctrl
        && binding.alt == modifiers.alt
        && binding.shift == modifiers.shift
}

fn consume_menu_visibility_shortcut(ctx: &egui::Context) -> bool {
    ctx.input_mut(|input| {
        input.consume_key(egui::Modifiers::CTRL | egui::Modifiers::SHIFT, egui::Key::M)
    })
}

fn consume_shortcut(ctx: &egui::Context, binding: &rshell_core::config::KeyBinding) -> bool {
    let key = ctx.input(|input| {
        input.events.iter().find_map(|event| match event {
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } if shortcut_matches(binding, *key, *modifiers) => Some((*key, false)),
            egui::Event::Copy if shortcut_matches(binding, egui::Key::C, input.modifiers) => {
                Some((egui::Key::C, true))
            }
            egui::Event::Cut if shortcut_matches(binding, egui::Key::X, input.modifiers) => {
                Some((egui::Key::X, true))
            }
            egui::Event::Paste(_) if shortcut_matches(binding, egui::Key::V, input.modifiers) => {
                Some((egui::Key::V, true))
            }
            _ => None,
        })
    });
    key.is_some_and(|(key, clipboard_event)| {
        if clipboard_event {
            return true;
        }
        let mut modifiers = egui::Modifiers::NONE;
        if binding.ctrl {
            modifiers |= egui::Modifiers::CTRL;
        }
        if binding.alt {
            modifiers |= egui::Modifiers::ALT;
        }
        if binding.shift {
            modifiers |= egui::Modifiers::SHIFT;
        }
        ctx.input_mut(|input| input.consume_key(modifiers, key))
    })
}

fn folder_picker(
    ui: &mut egui::Ui,
    config: &Config,
    selected: &mut Option<Uuid>,
    salt: impl std::hash::Hash + std::fmt::Debug,
) {
    egui::ComboBox::from_id_salt(salt)
        .selected_text(config.folder_path(*selected))
        .show_ui(ui, |ui| {
            ui.selectable_value(selected, None, "所有会话");
            let mut folders: Vec<_> = config
                .folders
                .iter()
                .map(|f| (f.id, config.folder_path(Some(f.id))))
                .collect();
            folders.sort_by(|a, b| a.1.cmp(&b.1));
            for (id, path) in folders {
                ui.selectable_value(selected, Some(id), path);
            }
        });
}

fn profile_matches(config: &Config, profile: &Profile, filter: &str) -> bool {
    format!(
        "{} {} {}",
        profile.name,
        profile.endpoint(),
        config.folder_path(profile.folder_id)
    )
    .to_lowercase()
    .contains(filter)
}

fn folder_matches(config: &Config, id: Uuid, filter: &str) -> bool {
    filter.is_empty()
        || config.folder_path(Some(id)).to_lowercase().contains(filter)
        || config
            .profiles
            .iter()
            .any(|p| p.folder_id == Some(id) && profile_matches(config, p, filter))
        || config
            .folders
            .iter()
            .any(|f| f.parent_id == Some(id) && folder_matches(config, f.id, filter))
}

#[derive(Clone, Copy)]
enum SessionDrag {
    Folder(Uuid),
    Profile(Uuid),
}

fn move_action(config: &Config, item: SessionDrag, destination: Option<Uuid>) -> Option<Action> {
    match item {
        SessionDrag::Profile(id) => {
            let profile = config.profiles.iter().find(|p| p.id == id)?;
            (profile.folder_id != destination).then_some(Action::MoveProfile {
                id,
                folder_id: destination,
            })
        }
        SessionDrag::Folder(id) => {
            let mut updated = config.folders.iter().find(|f| f.id == id)?.clone();
            if updated.parent_id == destination {
                return None;
            }
            updated.parent_id = destination;
            let mut candidate = config.clone();
            *candidate.folders.iter_mut().find(|f| f.id == id)? = updated.clone();
            candidate.validate_folders().ok()?;
            Some(Action::SaveFolder(updated))
        }
    }
}

fn folder_drop(
    ui: &mut egui::Ui,
    response: &egui::Response,
    config: &Config,
    destination: Option<Uuid>,
) -> Option<Action> {
    let payload = response.dnd_hover_payload::<SessionDrag>()?;
    let action = move_action(config, *payload, destination)?;
    ui.painter().rect_stroke(
        response.rect.shrink(1.0),
        3.0,
        egui::Stroke::new(2.0, ui.visuals().selection.bg_fill),
        egui::StrokeKind::Inside,
    );
    response.dnd_release_payload::<SessionDrag>()?;
    Some(action)
}

fn folder_icon(ui: &mut egui::Ui, openness: f32, response: &egui::Response) {
    let painter = ui.painter();
    let x = response.rect.left() - 2.0;
    let y = response.rect.center().y;
    let stroke = egui::Stroke::new(1.0, Color32::from_rgb(115, 149, 190));
    let points = if openness > 0.5 {
        [
            egui::pos2(x, y - 2.0),
            egui::pos2(x + 2.0, y + 1.0),
            egui::pos2(x + 4.0, y - 2.0),
        ]
    } else {
        [
            egui::pos2(x + 1.0, y - 3.0),
            egui::pos2(x + 3.0, y),
            egui::pos2(x + 1.0, y + 3.0),
        ]
    };
    painter.line_segment([points[0], points[1]], stroke);
    painter.line_segment([points[1], points[2]], stroke);
    painter.rect_stroke(
        egui::Rect::from_min_size(egui::pos2(x + 7.0, y - 3.0), egui::vec2(10.0, 8.0)),
        1.0,
        stroke,
        egui::StrokeKind::Inside,
    );
    painter.line_segment(
        [egui::pos2(x + 8.0, y - 3.0), egui::pos2(x + 8.0, y - 5.0)],
        stroke,
    );
    painter.line_segment(
        [egui::pos2(x + 8.0, y - 5.0), egui::pos2(x + 12.0, y - 5.0)],
        stroke,
    );
}

fn session_row(ui: &mut egui::Ui, name: &str, selected: bool, zoom: f32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 32.0 * zoom),
        egui::Sense::click_and_drag(),
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), name));
    let painter = ui.painter_at(rect);
    if selected || response.hovered() {
        painter.rect_filled(
            rect,
            0.0,
            if selected {
                ui.visuals().selection.bg_fill
            } else {
                ui.visuals().widgets.hovered.weak_bg_fill
            },
        );
    }
    let ink = ui.visuals().text_color();
    let x = rect.left() + 8.0 * zoom;
    let y = rect.center().y;
    let stroke = egui::Stroke::new(1.0, Color32::from_rgb(115, 149, 190));
    painter.rect_stroke(
        egui::Rect::from_min_size(
            egui::pos2(x, y - 6.0 * zoom),
            egui::vec2(14.0 * zoom, 11.0 * zoom),
        ),
        2.0,
        stroke,
        egui::StrokeKind::Inside,
    );
    painter.line_segment(
        [
            egui::pos2(x + 3.0 * zoom, y - 3.0 * zoom),
            egui::pos2(x + 6.0 * zoom, y - zoom),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(x + 6.0 * zoom, y - zoom),
            egui::pos2(x + 3.0 * zoom, y + zoom),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(x + 8.0 * zoom, y + 2.0 * zoom),
            egui::pos2(x + 11.0 * zoom, y + 2.0 * zoom),
        ],
        stroke,
    );
    painter.text(
        egui::pos2(x + 24.0 * zoom, y),
        egui::Align2::LEFT_CENTER,
        name,
        egui::FontId::proportional(15.0 * zoom),
        ink,
    );
    response
}

// Draw toolbar glyphs directly so they do not depend on installed symbol fonts.
fn toolbar_icon(ui: &mut egui::Ui, id: u8, zoom: f32) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(27.0 * zoom, 24.0 * zoom), egui::Sense::click());
    let painter = ui.painter_at(rect);
    if response.hovered() {
        painter.rect_filled(rect, 0.0, ui.visuals().widgets.hovered.weak_bg_fill);
    }
    let stroke = egui::Stroke::new(1.0, ui.visuals().text_color());
    let origin = rect.center() - egui::vec2(7.0 * zoom, 7.0 * zoom);
    let point = |x, y| origin + egui::vec2(x * zoom, y * zoom);
    let line = |a: (f32, f32), b: (f32, f32)| {
        painter.line_segment([point(a.0, a.1), point(b.0, b.1)], stroke);
    };
    let box_at = |x, y, w, h| {
        painter.rect_stroke(
            egui::Rect::from_min_size(point(x, y), egui::vec2(w * zoom, h * zoom)),
            0.0,
            stroke,
            egui::StrokeKind::Inside,
        );
    };
    match id {
        0 => {
            box_at(0.0, 1.0, 12.0, 10.0);
            line((8.0, 12.0), (14.0, 12.0));
            line((11.0, 9.0), (11.0, 15.0));
        }
        1 => {
            box_at(0.0, 1.0, 15.0, 12.0);
            line((3.0, 4.0), (6.0, 7.0));
            line((6.0, 7.0), (3.0, 10.0));
            line((8.0, 10.0), (12.0, 10.0));
        }
        2 => {
            painter.circle_stroke(point(7.0, 7.0), 5.0 * zoom, stroke);
            line((11.0, 0.0), (11.0, 5.0));
            line((11.0, 5.0), (6.0, 5.0));
        }
        3 => {
            line((3.0, 3.0), (11.0, 11.0));
            line((3.0, 11.0), (11.0, 3.0));
        }
        4 => {
            box_at(1.0, 1.0, 9.0, 10.0);
            box_at(5.0, 4.0, 9.0, 10.0);
        }
        5 => {
            painter.circle_stroke(point(6.0, 6.0), 4.5 * zoom, stroke);
            line((9.0, 9.0), (14.0, 14.0));
        }
        6 => {
            line((7.0, 1.0), (7.0, 10.0));
            line((3.0, 7.0), (7.0, 11.0));
            line((7.0, 11.0), (11.0, 7.0));
            line((1.0, 14.0), (13.0, 14.0));
        }
        7 => {
            box_at(0.0, 1.0, 15.0, 12.0);
            line((5.0, 1.0), (5.0, 13.0));
        }
        10 => {
            box_at(0.0, 3.0, 14.0, 10.0);
            line((1.0, 3.0), (1.0, 1.0));
            line((1.0, 1.0), (6.0, 1.0));
            line((6.0, 1.0), (8.0, 3.0));
            line((5.0, 8.0), (10.0, 8.0));
            line((7.5, 5.5), (7.5, 10.5));
        }
        _ => {
            for (y, x) in [(3.0, 4.0), (7.0, 10.0), (11.0, 6.0)] {
                line((0.0, y), (14.0, y));
                box_at(x, y - 2.0, 3.0, 4.0);
            }
        }
    }
    response
}

pub struct HostPrompt {
    fingerprint: String,
    public_key: String,
    reply: oneshot::Sender<bool>,
}

pub struct Tab {
    jumpserver: Option<jumpserver_view::SessionRef>,
    pub local: bool,
    pub font_size_override: Option<f32>,
    pub id: Uuid,
    pub profile: Profile,
    title: String,
    pub terminal: Arc<Mutex<Terminal>>,
    pub render_snapshot: Option<Arc<rshell_terminal::Snapshot>>,
    pub connection: Option<ssh::Connection>,
    pub status: String,
    pub tunnel_statuses: Vec<String>,
    pub owns_tunnels: bool,
    pub connected: bool,
    pub finished: bool,
    pub size: (u16, u16),
    pub preedit: String,
    pub search: String,
    pub show_search: bool,
    pub search_position: Option<(i32, usize)>,
    pub pending_paste: Option<String>,
    pub focus: bool,
    pub pointer_in_terminal: bool,
    pub pending_bottom: bool,
    pub scroll_remainder: f32,
    host_prompt: Option<HostPrompt>,
    transfer: transfer_view::Browser,
    #[cfg(test)]
    pub test_sent: Vec<ssh::Command>,
}

impl Tab {
    fn command_ready(&self) -> bool {
        self.connected
            && !self.profile.tunnel_only
            && (self.local || self.profile.protocol == Protocol::Ssh)
    }
    fn execution_identity(&self) -> String {
        if self.local {
            format!(
                "本地 Windows PowerShell · {} · 会话 {}（启动 Shell；手动切换后需重新确认）",
                self.title, self.id
            )
        } else {
            format!(
                "远端 SSH · {} · 会话 {}（Shell/权限未确认）",
                self.endpoint_label(),
                self.id
            )
        }
    }

    fn endpoint_label(&self) -> String {
        if let Some(reference) = &self.jumpserver {
            format!(
                "{}@{} ({}) · 经 JumpServer {}:{}",
                reference.account,
                reference.asset.name,
                reference.asset.address,
                reference.settings.ssh_host,
                reference.settings.ssh_port
            )
        } else {
            self.profile.endpoint()
        }
    }
    pub fn send(&mut self, cmd: ssh::Command) {
        if !self.connected || self.profile.tunnel_only {
            return;
        }
        #[cfg(test)]
        if self.connection.is_none() {
            self.test_sent.push(cmd);
            return;
        }
        if let Some(connection) = &self.connection
            && let Err(error) = connection.commands.try_send(cmd)
        {
            self.status = format!("发送失败，连接已停止：{error}");
            connection.disconnect();
            self.connected = false;
        }
    }
    pub fn input(&mut self, bytes: Vec<u8>) {
        // Output parsing owns this lock too. Never delay keyboard delivery for it.
        self.pending_bottom = true;
        if let Ok(mut terminal) = self.terminal.try_lock() {
            terminal.bottom();
            self.pending_bottom = false;
        }
        self.send(ssh::Command::Input(bytes));
    }
}

struct TerminalAdapter {
    terminal: Arc<Mutex<Terminal>>,
    ctx: egui::Context,
}
impl ssh::Terminal for TerminalAdapter {
    fn output(&mut self, data: &[u8]) -> Vec<u8> {
        let mut replies = Vec::new();
        for chunk in data.chunks(8192) {
            replies.extend(self.terminal.lock().unwrap().feed(chunk));
        }
        self.ctx.request_repaint_after(Duration::from_millis(16));
        replies
    }
    fn resize(&mut self, cols: u16, rows: u16) {
        self.terminal.lock().unwrap().resize(cols, rows);
        self.ctx.request_repaint();
    }
}

struct Editor {
    tunnel_settings: bool,
    show_secret: bool,
    focus_secret: bool,
    picker: Option<std::sync::mpsc::Receiver<Option<PathBuf>>>,
    serial_ports: Vec<String>,
    port_scan: Option<std::sync::mpsc::Receiver<Result<Vec<String>, String>>>,
    port_error: String,
    generation: Uuid,
    loading: bool,
    connect_after_load: bool,
    reconnect_tab: Option<Uuid>,
    profile: Profile,
    secret: String,
    loaded_identity: Option<(String, u16, String, AuthMethod, String)>,
}
impl Editor {
    fn new(profile: Profile) -> Self {
        Self {
            tunnel_settings: false,
            serial_ports: vec![],
            port_scan: None,
            port_error: String::new(),
            show_secret: false,
            focus_secret: !profile.host.is_empty(),
            picker: None,
            generation: Uuid::new_v4(),
            loading: false,
            connect_after_load: false,
            reconnect_tab: None,
            profile,
            secret: String::new(),
            loaded_identity: None,
        }
    }
    fn accept_secret(&mut self, secret: Option<Zeroizing<String>>) -> bool {
        self.loading = false;
        if let Some(secret) = secret {
            self.secret = secret.to_string();
            self.loaded_identity = Some(self.identity());
            self.connect_after_load && self.profile.validate().is_ok()
        } else {
            false
        }
    }
    fn enter_action(&self) -> i32 {
        if self.profile.remember_secret { 2 } else { 3 }
    }
    fn identity(&self) -> (String, u16, String, AuthMethod, String) {
        (
            self.profile.host.clone(),
            self.profile.port,
            self.profile.username.clone(),
            self.profile.auth.clone(),
            self.profile.key_path.clone(),
        )
    }
}
impl Drop for Editor {
    fn drop(&mut self) {
        self.secret.zeroize();
    }
}

#[derive(Clone, Copy)]
enum CloseTabs {
    Current,
    Right,
    Local,
    Disconnected,
    Others,
    All,
}

fn close_tabs(tabs: &mut Vec<Tab>, active: &mut usize, target: usize, mode: CloseTabs) {
    if target >= tabs.len() {
        return;
    }
    let selected = tabs.get(*active).map(|tab| tab.id);
    let mut index = 0;
    tabs.retain(|tab| {
        let remove = match mode {
            CloseTabs::Current => index == target,
            CloseTabs::Right => index > target,
            CloseTabs::Local => tab.local,
            CloseTabs::Disconnected => !tab.local && tab.finished && !tab.connected,
            CloseTabs::Others => index != target,
            CloseTabs::All => true,
        };
        index += 1;
        !remove
    });
    *active = tabs
        .iter()
        .position(|tab| Some(tab.id) == selected)
        .unwrap_or(target.min(tabs.len().saturating_sub(1)));
    if let Some(tab) = tabs.get_mut(*active) {
        tab.focus = true;
    }
}

fn close_startup_local_tab(tabs: &mut Vec<Tab>, active: &mut usize, id: Uuid) {
    if let Some(index) = tabs.iter().position(|tab| tab.id == id) {
        close_tabs(tabs, active, index, CloseTabs::Current);
    }
}

fn adjacent_tab(active: usize, count: usize, direction: isize) -> usize {
    if count == 0 {
        0
    } else {
        (active as isize + direction).rem_euclid(count as isize) as usize
    }
}

enum TabAction {
    New,
    Duplicate,
    Channel,
    ClearScreen,
    Close(CloseTabs),
    Rename,
}

fn sidebar_zoom_steps(events: &[egui::Event]) -> f32 {
    events
        .iter()
        .filter_map(|event| {
            if let egui::Event::MouseWheel {
                unit,
                delta,
                modifiers,
                ..
            } = event
                && modifiers.ctrl
            {
                Some(
                    delta.y
                        * match unit {
                            egui::MouseWheelUnit::Point => 1.0 / 50.0,
                            egui::MouseWheelUnit::Line => 1.0,
                            egui::MouseWheelUnit::Page => 3.0,
                        },
                )
            } else {
                None
            }
        })
        .sum()
}

fn restore_terminal_focus_after_dialog(
    was_blocked: &mut bool,
    blocked: bool,
    tab: Option<&mut Tab>,
) {
    if *was_blocked
        && !blocked
        && let Some(tab) = tab
    {
        tab.focus = true;
    }
    *was_blocked = blocked;
}

fn clear_selection_on_primary_click(ctx: &egui::Context, tab: Option<&mut Tab>) {
    if ctx.input(|input| input.pointer.button_pressed(egui::PointerButton::Primary))
        && let Some(tab) = tab
    {
        tab.terminal.lock().unwrap().clear_selection();
    }
}

fn apply_ui_appearance(ctx: &egui::Context, config: &Config) {
    ctx.set_zoom_factor(config.ui_zoom);
    ctx.all_styles_mut(|style| {
        style.text_styles.insert(
            egui::TextStyle::Body,
            egui::FontId::proportional(config.ui_font_size),
        );
        style.text_styles.insert(
            egui::TextStyle::Button,
            egui::FontId::proportional(config.ui_font_size),
        );
        style.text_styles.insert(
            egui::TextStyle::Heading,
            egui::FontId::proportional(config.ui_font_size + 5.0),
        );
        style.text_styles.insert(
            egui::TextStyle::Small,
            egui::FontId::proportional(config.ui_font_size - 2.0),
        );
    });
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum TerminalLayout {
    #[default]
    Single,
    Columns,
    Rows,
    Grid,
}

#[derive(Clone)]
struct PaneSplits {
    layout: TerminalLayout,
    count: usize,
    columns: Vec<f32>,
    rows: Vec<f32>,
}

impl TerminalLayout {
    fn rects(self, ui: &mut egui::Ui, count: usize, allow_input: bool) -> (Vec<egui::Rect>, bool) {
        if count == 0 {
            return (vec![], false);
        }
        let area = ui.available_rect_before_wrap();
        let columns = match self {
            Self::Single | Self::Rows => 1,
            Self::Columns => count,
            Self::Grid => (count as f32).sqrt().ceil() as usize,
        };
        let rows = count.div_ceil(columns);
        let id = ui.scope_id().with("terminal_split_sizes");
        let mut sizes = ui
            .data_mut(|data| data.get_temp::<PaneSplits>(id))
            .filter(|sizes| sizes.layout == self && sizes.count == count)
            .unwrap_or_else(|| PaneSplits {
                layout: self,
                count,
                columns: (0..=columns).map(|n| n as f32 / columns as f32).collect(),
                rows: (0..=rows).map(|n| n as f32 / rows as f32).collect(),
            });
        let mut resizing = false;
        for (horizontal, boundaries) in [(true, &mut sizes.columns), (false, &mut sizes.rows)] {
            let length = if horizontal {
                area.width()
            } else {
                area.height()
            };
            if length <= 0.0 {
                continue;
            }
            let minimum = (80.0 / length).min(0.5 / (boundaries.len() - 1) as f32);
            for index in 1..boundaries.len() - 1 {
                let coordinate =
                    if horizontal { area.left() } else { area.top() } + length * boundaries[index];
                let handle = if horizontal {
                    egui::Rect::from_min_max(
                        egui::pos2(coordinate - 4.0, area.top()),
                        egui::pos2(coordinate + 4.0, area.bottom()),
                    )
                } else {
                    egui::Rect::from_min_max(
                        egui::pos2(area.left(), coordinate - 4.0),
                        egui::pos2(area.right(), coordinate + 4.0),
                    )
                };
                let response = ui.interact(
                    handle,
                    id.with((horizontal, index)),
                    if allow_input {
                        egui::Sense::drag()
                    } else {
                        egui::Sense::hover()
                    },
                );
                if allow_input && (response.hovered() || response.dragged()) {
                    ui.ctx().set_cursor_icon(if horizontal {
                        egui::CursorIcon::ResizeHorizontal
                    } else {
                        egui::CursorIcon::ResizeVertical
                    });
                    resizing |=
                        response.dragged() || ui.input(|input| input.pointer.primary_down());
                }
                if allow_input && response.dragged() {
                    // Apply this frame's delta, preserving the pointer's grab offset.
                    let delta = ui.input(|input| {
                        if horizontal {
                            input.pointer.delta().x
                        } else {
                            input.pointer.delta().y
                        }
                    });
                    let minimum =
                        minimum.min((boundaries[index + 1] - boundaries[index - 1]) * 0.5);
                    boundaries[index] = (boundaries[index] + delta / length).clamp(
                        boundaries[index - 1] + minimum,
                        boundaries[index + 1] - minimum,
                    );
                }
                let color = if response.hovered() || response.dragged() {
                    ui.visuals().selection.bg_fill
                } else {
                    ui.visuals().widgets.noninteractive.bg_stroke.color
                };
                let coordinate =
                    if horizontal { area.left() } else { area.top() } + length * boundaries[index];
                if horizontal {
                    ui.painter()
                        .vline(coordinate, area.y_range(), egui::Stroke::new(2.0, color));
                } else {
                    ui.painter()
                        .hline(area.x_range(), coordinate, egui::Stroke::new(2.0, color));
                }
            }
        }
        let rects = (0..count)
            .map(|index| {
                let col = index % columns;
                let row = index / columns;
                egui::Rect::from_min_max(
                    area.min
                        + egui::vec2(
                            sizes.columns[col] * area.width(),
                            sizes.rows[row] * area.height(),
                        ),
                    area.min
                        + egui::vec2(
                            sizes.columns[col + 1] * area.width(),
                            sizes.rows[row + 1] * area.height(),
                        ),
                )
                .shrink(4.0)
            })
            .collect();
        ui.data_mut(|data| data.insert_temp(id, sizes));
        (rects, resizing)
    }
}

pub struct App {
    jumpserver: jumpserver_view::State,
    session_file_picker: Option<std::sync::mpsc::Receiver<Option<SessionFilePick>>>,
    import_open: bool,
    import_path: String,
    import_result: String,
    import_picker: Option<std::sync::mpsc::Receiver<Option<PathBuf>>>,
    rename: Option<(Uuid, String)>,
    host_editor: Option<HostEditor>,
    keys: key_manager::KeyManager,
    config: Config,
    path: PathBuf,
    runtime: tokio::runtime::Runtime,
    tabs: Vec<Tab>,
    terminal_layout: TerminalLayout,
    pane_groups: PaneGroups,
    startup_local_tab: Option<Uuid>,
    active: usize,
    visible_tab: Option<Uuid>,
    tab_strip_width: f32,
    tab_scroll_offset: f32,
    shortcut_offset: f32,
    filter: String,
    sessions_zoom: f32,
    address: String,
    show_sessions: bool,
    folder_editor: Option<Folder>,
    editor: Option<Editor>,
    message: String,
    delete: Option<Uuid>,
    settings: bool,
    settings_page: settings_view::SettingsPage,
    shortcut_capture: Option<usize>,
    scrollback_draft: Option<usize>,
    keep_alive_draft: Option<(Uuid, rshell_core::config::KeepAlive)>,
    show_ai_chat: bool,
    ai_draft: String,
    ai_messages: Vec<rshell_core::ai::ChatMessage>,
    ai_history: rshell_core::ai::ChatHistory,
    ai_history_loaded: bool,
    ai_thread_id: Option<Uuid>,
    ai_show_history: bool,
    ai_selected_connection: Option<Uuid>,
    ai_command_mode: bool,
    agent_mode: bool,
    agent_execute: bool,
    agent_context: bool,
    agent_context_preview: bool,
    agent: Option<agent_view::Run>,
    claude_draft: Option<rshell_core::claude_agent::Settings>,
    ai_maximized: bool,
    ai_export: Option<std::sync::mpsc::Receiver<Result<Option<std::path::PathBuf>, String>>>,
    ai_notice: String,
    ai_settings_filter: String,
    ai_blocklist_draft: Option<String>,
    ai_request: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
    ai_request_abort: Option<tokio::task::AbortHandle>,
    ai_error: String,
    ai_generation: Uuid,
    ai_command: Option<ai_settings::AiCommand>,
    ai_editor: Option<ai_settings::AiEditor>,
    ai_delete: Option<Uuid>,
    smoke: bool,
    started: Instant,
    screenshot_requested: bool,
    store: Store,
    pending: HashMap<Uuid, Pending>,
    close_when_idle: bool,
    exit_approved: bool,
    confirm_exit_open: bool,
    pending_close: Option<(usize, CloseTabs)>,
    pending_disconnect: Option<Uuid>,
    terminal_focus_blocked: bool,
    updates: update_view::UpdateState,
    about_open: bool,
}

struct HostEditor {
    original: KnownHost,
    public_key: String,
    fingerprint: String,
    verified: bool,
    error: String,
}

enum Pending {
    JumpToken(Uuid),
    Import,
    SessionFile,
    SaveProfile {
        editor: Box<Editor>,
        connect: bool,
    },
    ReadSecret {
        generation: Uuid,
    },
    AiSave(Box<ai_settings::AiEditor>),
    AiUpdate,
    AgentKey {
        generation: Uuid,
        request: Box<agent_view::Request>,
    },
    AiKey {
        profile: rshell_core::ai::AiProfile,
        generation: Uuid,
        prompt: rshell_core::ai::ChatMessage,
    },
    AiHistoryLoad,
    AiHistorySave,
    Host,
    Delete,
    Update,
    HostEdit(Box<HostEditor>),
    Folder(Folder),
}

enum SessionFilePick {
    Import(PathBuf),
    ExportRsh(PathBuf),
    ExportXts(PathBuf),
}

impl Drop for App {
    fn drop(&mut self) {
        // Cancel blocking ConPTY workers before the Tokio runtime waits for them.
        self.tabs.clear();
    }
}

impl App {
    fn choose_session_file(&mut self, ctx: &egui::Context, export: Option<bool>) {
        if self.session_file_picker.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let notify = ctx.clone();
        std::thread::spawn(move || {
            let selected = match export {
                Some(true) => rfd::FileDialog::new()
                    .set_title("导出全部 SSH/Telnet 会话")
                    .add_filter("Xshell 导出", &["xts"])
                    .set_file_name("sessions.xts")
                    .save_file()
                    .map(SessionFilePick::ExportXts),
                Some(false) => rfd::FileDialog::new()
                    .set_title("导出全部 RShell 会话")
                    .add_filter("RShell 会话", &["rsh"])
                    .set_file_name("sessions.rsh")
                    .save_file()
                    .map(SessionFilePick::ExportRsh),
                None => rfd::FileDialog::new()
                    .set_title("导入 RShell 会话")
                    .add_filter("RShell 会话", &["rsh"])
                    .pick_file()
                    .map(SessionFilePick::Import),
            };
            let _ = tx.send(selected);
            notify.request_repaint();
        });
        self.session_file_picker = Some(rx);
    }

    fn poll_session_file_picker(&mut self) {
        let Some(receiver) = &self.session_file_picker else {
            return;
        };
        match receiver.try_recv() {
            Ok(pick) => {
                self.session_file_picker = None;
                if let Some(pick) = pick {
                    let action = match pick {
                        SessionFilePick::Import(path) => Action::ImportRsh(path),
                        SessionFilePick::ExportRsh(path) => Action::ExportRsh { path },
                        SessionFilePick::ExportXts(path) => Action::ExportXts { path },
                    };
                    self.submit(action, Pending::SessionFile);
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => self.session_file_picker = None,
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
    }
    fn new_profile(&self) -> Profile {
        Profile {
            remember_secret: self.config.session_options.remember_secret_by_default,
            ..Default::default()
        }
    }

    fn request_close_tabs(&mut self, target: usize, mode: CloseTabs) {
        if target >= self.tabs.len() {
            return;
        }
        let needs_confirmation = self.tabs.iter().enumerate().any(|(index, tab)| {
            let selected = match mode {
                CloseTabs::Current => index == target,
                CloseTabs::Right => index > target,
                CloseTabs::Local => tab.local,
                CloseTabs::Disconnected => !tab.local && tab.finished && !tab.connected,
                CloseTabs::Others => index != target,
                CloseTabs::All => true,
            };
            selected && tab.connected && !tab.local
        });
        if needs_confirmation && self.config.session_options.confirm_disconnect {
            self.pending_close = Some((target, mode));
        } else {
            close_tabs(&mut self.tabs, &mut self.active, target, mode);
        }
    }

    fn session_confirmations(&mut self, ctx: &egui::Context) {
        if self.confirm_exit_open {
            egui::Modal::new(egui::Id::unique("confirm_exit")).show(ctx, |ui| {
                ui.heading("退出 RShell？");
                ui.label("正在运行的会话将断开连接。");
                ui.horizontal(|ui| {
                    if ui.button("取消").clicked() {
                        self.confirm_exit_open = false;
                    }
                    if ui.button("断开并退出").clicked() {
                        self.confirm_exit_open = false;
                        self.exit_approved = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });
        }
        if let Some((target, mode)) = self.pending_close {
            egui::Modal::new(egui::Id::unique("confirm_close_tabs")).show(ctx, |ui| {
                ui.heading("终止会话？");
                ui.label("选中的连接将断开。");
                ui.horizontal(|ui| {
                    if ui.button("取消").clicked() {
                        self.pending_close = None;
                    }
                    if ui.button("断开并关闭").clicked() {
                        self.pending_close = None;
                        close_tabs(&mut self.tabs, &mut self.active, target, mode);
                    }
                });
            });
        }
        if let Some(id) = self.pending_disconnect {
            egui::Modal::new(egui::Id::unique("confirm_disconnect")).show(ctx, |ui| {
                ui.heading("断开连接？");
                ui.horizontal(|ui| {
                    if ui.button("取消").clicked() {
                        self.pending_disconnect = None;
                    }
                    if ui.button("断开").clicked() {
                        self.pending_disconnect = None;
                        if let Some(tab) = self.tabs.iter().find(|tab| tab.id == id)
                            && let Some(connection) = &tab.connection
                        {
                            connection.disconnect();
                        }
                    }
                });
            });
        }
    }

    fn import_dialog(&mut self, ctx: &egui::Context) {
        if let Some(receiver) = &self.import_picker {
            match receiver.try_recv() {
                Ok(path) => {
                    if let Some(path) = path {
                        self.import_path = path.to_string_lossy().into_owned();
                    }
                    self.import_picker = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.import_picker = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if !self.import_open {
            return;
        }
        let busy = self.pending.values().any(|p| matches!(p, Pending::Import));
        let mut open = true;
        let mut import = false;
        let mut repair = false;
        egui::Window::new("导入 Xshell 会话")
            .open(&mut open).collapsible(false).default_width(580.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("导入 SSH / Telnet 配置与文件夹；新建独立导入目录，不覆盖现有会话。");
                ui.label("不导入密码、私钥、脚本、代理、转发或主机信任。SSH 缺少用户名时需补填。Telnet 使用明文传输，登录信息在终端输入。");
                ui.label("串口枚举参数尚不兼容，将跳过并计数。仅支持 UTF-8 / UTF-16 会话。");
                ui.label("文件名支持 UTF-8 / GBK。已有中文乱码：选择原 XTS 后点「修复已导入名称」，无需重复导入。");
                ui.add_enabled_ui(!busy && self.import_picker.is_none(), |ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.import_path).desired_width(420.0).hint_text("Xshell 导出的 .xts 文件路径"));
                        if ui.button("浏览…").clicked() {
                            let (tx, rx) = std::sync::mpsc::sync_channel(1);
                            let notify = ctx.clone();
                            std::thread::spawn(move || {
                                let path = rfd::FileDialog::new().set_title("选择 Xshell 导出文件").add_filter("Xshell 导出", &["xts"]).pick_file();
                                let _ = tx.send(path); notify.request_repaint();
                            });
                            self.import_picker = Some(rx);
                        }
                    });
                    import = ui.add_enabled(!self.import_path.trim().is_empty(), egui::Button::new("导入配置和文件夹")).clicked();
                    repair = ui.add_enabled(!self.import_path.trim().is_empty(), egui::Button::new("修复已导入名称（不新增会话）")).clicked();
                });
                if busy { ui.spinner(); ui.label("正在后台导入并保存…"); }
                if !self.import_result.is_empty() { ui.separator(); ui.label(&self.import_result); }
            });
        self.import_open = open;
        if import || repair {
            self.import_result.clear();
            self.submit(
                if repair {
                    Action::RepairXtsNames(PathBuf::from(self.import_path.trim()))
                } else {
                    Action::ImportXts(PathBuf::from(self.import_path.trim()))
                },
                Pending::Import,
            );
        }
    }

    pub fn new(
        cc: &eframe::CreationContext<'_>,
        mut config: Config,
        path: PathBuf,
        runtime: tokio::runtime::Runtime,
        demo: bool,
        smoke: bool,
    ) -> anyhow::Result<Self> {
        egui_extras::install_image_loaders(&cc.egui_ctx);
        if smoke && std::env::args().any(|a| a == "--smoke-light") {
            config.light_theme = true;
        }
        if smoke && std::env::args().any(|a| a == "--smoke-ai-font-large") {
            config.ai.chat_font_size = 24.0;
        }
        if smoke && std::env::args().any(|a| a == "--smoke-hidden-chrome") {
            config.chrome = rshell_core::config::ChromeVisibility {
                menu: false,
                toolbar: false,
                address: false,
                shortcuts: false,
            };
        }
        cc.egui_ctx.set_theme(if config.light_theme {
            egui::ThemePreference::Light
        } else {
            egui::ThemePreference::Dark
        });
        let notify = cc.egui_ctx.clone();
        let store = Store::start(
            path.clone(),
            config.clone(),
            rshell_core::credentials::EncryptedCredentials(
                path.parent().unwrap().join("credentials"),
            ),
            move || notify.request_repaint(),
        )?;
        let sessions_zoom = config.sidebar_zoom;
        let jumpserver = jumpserver_view::State::new(config.jumpserver.as_ref());
        let mut app = Self {
            jumpserver,
            session_file_picker: None,
            import_open: smoke && std::env::args().any(|a| a == "--smoke-import"),
            import_path: String::new(),
            import_result: String::new(),
            import_picker: None,
            rename: None,
            host_editor: None,
            keys: key_manager::KeyManager::default(),
            config,
            path,
            runtime,
            tabs: vec![],
            terminal_layout: TerminalLayout::Single,
            pane_groups: PaneGroups::default(),
            startup_local_tab: None,
            active: 0,
            visible_tab: None,
            tab_strip_width: 0.0,
            tab_scroll_offset: 0.0,
            shortcut_offset: 0.0,
            filter: String::new(),
            sessions_zoom,
            address: String::new(),
            show_sessions: true,
            folder_editor: None,
            editor: None,
            message: String::new(),
            delete: None,
            settings: std::env::args().any(|a| a == "--ai-settings")
                || (smoke && std::env::args().any(|a| a == "--smoke-history-settings"))
                || (smoke && std::env::args().any(|a| a == "--smoke-appearance"))
                || (smoke && std::env::args().any(|a| a == "--smoke-shortcuts"))
                || (smoke && std::env::args().any(|a| a == "--smoke-session-options")),
            settings_page: if std::env::args().any(|a| a == "--ai-settings") {
                settings_view::SettingsPage::Ai
            } else if smoke && std::env::args().any(|a| a == "--smoke-shortcuts") {
                settings_view::SettingsPage::Shortcuts
            } else if smoke && std::env::args().any(|a| a == "--smoke-session-options") {
                settings_view::SettingsPage::Session
            } else {
                settings_view::SettingsPage::Appearance
            },
            shortcut_capture: None,
            scrollback_draft: None,
            keep_alive_draft: None,
            show_ai_chat: smoke && std::env::args().any(|a| a == "--smoke-ai-chat"),
            ai_draft: if smoke && std::env::args().any(|a| a == "--smoke-ai-long-draft") {
                "模拟选中的终端输出：deployment.yaml scripts/ 日志内容\n".repeat(100)
            } else {
                String::new()
            },
            ai_messages: Vec::new(),
            ai_history: rshell_core::ai::ChatHistory::default(),
            ai_history_loaded: false,
            ai_thread_id: None,
            ai_show_history: false,
            ai_selected_connection: None,
            ai_command_mode: false,
            agent_mode: true,
            agent_execute: smoke && std::env::args().any(|a| a == "--smoke-agent-execute"),
            agent_context: true,
            agent_context_preview: false,
            agent: None,
            claude_draft: None,
            ai_maximized: smoke && std::env::args().any(|a| a == "--smoke-ai-maximized"),
            ai_export: None,
            ai_notice: String::new(),
            ai_settings_filter: String::new(),
            ai_blocklist_draft: None,
            ai_request: None,
            ai_request_abort: None,
            ai_error: String::new(),
            ai_generation: Uuid::new_v4(),
            ai_command: None,
            ai_editor: None,
            ai_delete: None,
            smoke,
            started: Instant::now(),
            screenshot_requested: false,
            store,
            pending: HashMap::new(),
            close_when_idle: false,
            exit_approved: false,
            confirm_exit_open: false,
            pending_close: None,
            pending_disconnect: None,
            terminal_focus_blocked: false,
            updates: update_view::UpdateState::default(),
            about_open: false,
        };
        if demo {
            let tab = app.configured_tab(Profile {
                name: "终端演示 · 无远程连接".into(),
                ..Default::default()
            });
            tab.terminal.lock().unwrap().feed(
                concat!(
                    "\x1b[1;36mRShell\x1b[0m  ·  Native Rust terminal\r\n\r\n",
                    "\x1b[32m+\x1b[0m 原生 Windows 窗口 / SSH / UTF-8 / ANSI\r\n",
                    "\x1b[33m演示模式：这些内容来自本地测试数据，没有连接服务器。\x1b[0m\r\n\r\n",
                    "\x1b[34muser@server\x1b[0m:~$ ls\r\n",
                    "\x1b[36mdeploy/    logs/    scripts/\x1b[0m    README.md\r\n\r\n",
                    "中文输入、彩色输出、标签会话。\r\n",
                    "左侧新建会话，即可输入真实服务器连接信息。\r\n"
                )
                .as_bytes(),
            );
            app.tabs.push(tab);
        }
        if smoke && std::env::args().any(|a| a == "--smoke-terminal-search") {
            if let Some(tab) = app.tabs.get_mut(app.active) {
                tab.show_search = true;
                tab.search = "Rust".into();
            }
            cc.egui_ctx
                .send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(800.0, 520.0)));
        }
        let tab_smoke = smoke
            && std::env::args()
                .any(|arg| arg == "--smoke-tabs-fit" || arg == "--smoke-tabs-overflow");
        if tab_smoke {
            app.tabs[0].title = "online-NEW-K8S-PRO-20210224 172.18.234.153".into();
            let count = if std::env::args().any(|arg| arg == "--smoke-tabs-fit") {
                4
            } else {
                9
            };
            for index in 2..=count {
                let mut tab = app.configured_tab(Profile::default());
                tab.title = format!("online-new-k8s-{index:08} · 172.18.22.{index}");
                app.tabs.push(tab);
            }
            app.active = if count == 9 { count - 1 } else { 0 };
        }
        if smoke && std::env::args().any(|a| a == "--smoke-split") {
            app.terminal_layout = TerminalLayout::Grid;
            while app.tabs.len() < 8 {
                let tab = app.configured_tab(Profile {
                    name: format!("分屏演示 {}", app.tabs.len() + 1),
                    ..Default::default()
                });
                tab.terminal
                    .lock()
                    .unwrap()
                    .feed(b"Independent terminal pane\r\n$ ");
                app.tabs.push(tab);
            }
        }
        if smoke
            && std::env::args().any(|a| a == "--smoke-sftp")
            && let Some(tab) = app.tabs.first_mut()
        {
            tab.transfer.smoke_fixture();
        }
        if smoke && std::env::args().any(|a| a == "--smoke-serial") {
            app.editor = Some(Editor::new(Profile {
                protocol: Protocol::Serial,
                ..Default::default()
            }));
        }
        if smoke && std::env::args().any(|a| a == "--smoke-session") {
            app.editor = Some(Editor::new(app.new_profile()));
        }
        if smoke && std::env::args().any(|a| a == "--smoke-tunnels") {
            let mut editor = Editor::new(app.new_profile());
            editor.tunnel_settings = true;
            editor.profile.tunnel_only = true;
            editor.profile.tunnels = vec![
                rshell_core::tunnel::Rule::default(),
                rshell_core::tunnel::Rule {
                    kind: rshell_core::tunnel::Kind::Dynamic,
                    bind_port: 1080,
                    ..Default::default()
                },
            ];
            app.editor = Some(editor);
        }
        if smoke && std::env::args().any(|a| a == "--smoke-tunnel-status") {
            let tab = &mut app.tabs[0];
            tab.profile.tunnel_only = true;
            tab.profile.tunnels = vec![rshell_core::tunnel::Rule::default()];
            tab.tunnel_statuses = vec!["演示状态：监听中（未建立真实隧道）".into()];
            cc.egui_ctx
                .send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(800.0, 520.0)));
        }
        if smoke && std::env::args().any(|a| a == "--smoke-telnet") {
            app.editor = Some(Editor::new(Profile {
                protocol: Protocol::Telnet,
                port: 23,
                ..Default::default()
            }));
        }
        if !demo {
            app.start_local(&cc.egui_ctx, None);
            app.startup_local_tab = app.tabs.last().map(|tab| tab.id);
        }
        cc.egui_ctx.all_styles_mut(|style| {
            style.spacing.item_spacing = egui::vec2(4.0, 3.0);
            style.spacing.button_padding = egui::vec2(6.0, 3.0);
            style.spacing.interact_size.y = 26.0;
            style.visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(4);
            style.visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(4);
            style.visuals.widgets.active.corner_radius = egui::CornerRadius::same(4);
            style
                .text_styles
                .insert(egui::TextStyle::Body, egui::FontId::proportional(13.0));
            style
                .text_styles
                .insert(egui::TextStyle::Button, egui::FontId::proportional(13.0));
            style.visuals.selection.bg_fill = Color32::from_rgb(55, 164, 232);
            style.visuals.widgets.active.bg_fill = Color32::from_rgb(55, 164, 232);
        });
        apply_ui_appearance(&cc.egui_ctx, &app.config);
        cc.egui_ctx.style_mut_of(egui::Theme::Dark, |style| {
            style.visuals.panel_fill = Color32::from_rgb(23, 29, 40);
            style.visuals.window_fill = Color32::from_rgb(29, 37, 50);
            style.visuals.extreme_bg_color = Color32::from_rgb(16, 22, 32);
            style.visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(35, 45, 60);
            style.visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(43, 61, 79);
            style.visuals.window_stroke = egui::Stroke::new(1.0, Color32::from_rgb(54, 70, 90));
        });
        cc.egui_ctx.style_mut_of(egui::Theme::Light, |style| {
            let border = egui::Stroke::new(1.0, Color32::from_rgb(193, 210, 229));
            let ink = Color32::from_rgb(43, 60, 79);
            style.visuals.panel_fill = Color32::from_rgb(225, 235, 247);
            style.visuals.window_fill = Color32::from_rgb(237, 243, 250);
            style.visuals.extreme_bg_color = Color32::from_rgb(247, 250, 255);
            style.visuals.faint_bg_color = Color32::from_rgb(232, 240, 250);
            style.visuals.override_text_color = Some(ink);
            style.visuals.window_stroke = border;
            style.visuals.window_corner_radius = egui::CornerRadius::same(6);
            style.visuals.widgets.noninteractive.bg_stroke = border;
            style.visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, ink);
            style.visuals.widgets.inactive.bg_fill = Color32::from_rgb(237, 244, 252);
            style.visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
            style.visuals.widgets.inactive.bg_stroke = border;
            style.visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, ink);
            style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(214, 229, 247);
            style.visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(214, 229, 247);
            style.visuals.widgets.hovered.bg_stroke = border;
            style.visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, ink);
            style.visuals.widgets.active.bg_fill = Color32::from_rgb(198, 218, 242);
            style.visuals.widgets.active.weak_bg_fill = Color32::from_rgb(198, 218, 242);
            style.visuals.widgets.active.bg_stroke = border;
            style.visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, ink);
            style.visuals.selection.bg_fill = Color32::from_rgb(202, 222, 245);
            style.visuals.selection.stroke = egui::Stroke::new(1.0, ink);
        });
        crate::ui_appearance::apply_palette(&cc.egui_ctx, &app.config);
        if !app.submit(Action::ReadAiHistory, Pending::AiHistoryLoad) {
            app.ai_history_loaded = true;
            app.ai_error = std::mem::take(&mut app.message);
        }
        Ok(app)
    }

    fn chrome_action(&mut self, action: u8, ctx: &egui::Context) {
        match action {
            16 => {
                let mut profile = self.new_profile();
                profile.name = "SSH 隧道".into();
                profile.tunnel_only = true;
                profile.tunnels.push(rshell_core::tunnel::Rule::default());
                self.open_profile_editor(profile, false, None);
            }
            15 => self.jumpserver.open = true,
            14 => self.choose_session_file(ctx, None),
            13 => self.import_open = true,
            0 => self.open_profile(self.new_profile()),
            1 => self.start_local(ctx, None),
            2 => {
                if let Some(tab) = self.tabs.get(self.active) {
                    if tab.local {
                        self.start_local(ctx, Some(tab.id));
                    } else if let Some(mut reference) = tab.jumpserver.clone() {
                        reference.reconnect_tab = Some(tab.id);
                        self.reconnect_jumpserver(ctx, reference);
                    } else {
                        self.open_profile_editor(tab.profile.clone(), true, Some(tab.id));
                    }
                }
            }
            3..=6 => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    match action {
                        3 => {
                            if tab.connected
                                && !tab.local
                                && self.config.session_options.confirm_disconnect
                            {
                                self.pending_disconnect = Some(tab.id);
                            } else if let Some(connection) = &tab.connection {
                                connection.disconnect();
                            }
                        }
                        4 => {
                            ctx.copy_text(tab.terminal.lock().unwrap().selection_text());
                            tab.focus = true;
                        }
                        5 => tab.show_search = !tab.show_search,
                        6 => {
                            tab.terminal.lock().unwrap().bottom();
                            tab.focus = true;
                        }
                        _ => unreachable!(),
                    }
                }
            }
            7 => self.show_sessions = !self.show_sessions,
            8 => {
                if !self.tabs.is_empty() {
                    let target = self.active;
                    self.request_close_tabs(target, CloseTabs::Current);
                }
            }
            9 => self.settings = true,
            11 => self.show_ai_chat = !self.show_ai_chat,
            12 => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    if tab.jumpserver.is_some() {
                        self.message =
                            "JumpServer 会话的 SFTP 尚未验收，请使用 JumpServer 文件管理".into();
                    } else if tab.connected && !tab.local && tab.profile.protocol == Protocol::Ssh {
                        tab.transfer.open = true;
                    } else {
                        self.message = "请先连接 SSH 会话，再打开 SFTP".into();
                    }
                }
            }
            _ => {}
        }
    }

    fn tab_action(&mut self, target: usize, action: TabAction, ctx: &egui::Context) {
        let Some(tab) = self.tabs.get(target) else {
            return;
        };
        match action {
            TabAction::New => self.start_local(ctx, None),
            TabAction::Duplicate => {
                if tab.local {
                    self.start_local(ctx, None);
                } else if let Some(reference) = tab.jumpserver.clone() {
                    self.reconnect_jumpserver(ctx, reference);
                } else {
                    self.open_profile(tab.profile.clone());
                }
            }
            TabAction::Channel => {
                if let Some(reference) = tab.jumpserver.clone() {
                    self.reconnect_jumpserver(ctx, reference);
                    return;
                }
                if tab.local
                    || tab.profile.protocol != Protocol::Ssh
                    || !tab.connected
                    || tab.profile.tunnel_only
                {
                    return;
                }
                let Some(source) = &tab.connection else {
                    return;
                };
                let mut new = self.configured_tab(tab.profile.clone());
                // A duplicate shell channel does not own or start the original tab's listeners.
                new.owns_tunnels = false;
                let adapter = TerminalAdapter {
                    terminal: new.terminal.clone(),
                    ctx: ctx.clone(),
                };
                match ssh::duplicate_channel(self.runtime.handle(), source, adapter) {
                    Ok(connection) => {
                        new.connection = Some(connection);
                        new.status = "正在打开 SSH 渠道".into();
                        self.tabs.push(new);
                        self.active = self.tabs.len() - 1;
                    }
                    Err(error) => self.message = error.to_string(),
                }
            }
            TabAction::Close(mode) => self.request_close_tabs(target, mode),
            TabAction::ClearScreen => self.tabs[target].terminal.lock().unwrap().clear_screen(),
            TabAction::Rename => self.rename = Some((tab.id, tab.title.clone())),
        }
    }

    fn rename_dialog(&mut self, ctx: &egui::Context) {
        let Some((id, name)) = &mut self.rename else {
            return;
        };
        let mut accept = false;
        let mut cancel = false;
        egui::Modal::new(egui::Id::unique("rename_tab")).show(ctx, |ui| {
            ui.heading("重命名标签");
            ui.label("仅修改当前标签标题，保存的会话名称不变。");
            let response = ui.add(egui::TextEdit::singleline(name).char_limit(128));
            if !response.has_focus() {
                response.request_focus();
            }
            ui.horizontal(|ui| {
                accept = ui
                    .add_enabled(!name.trim().is_empty(), egui::Button::new("确定"))
                    .clicked()
                    || (!name.trim().is_empty() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                cancel =
                    ui.button("取消").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape));
            });
        });
        if accept && let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == *id) {
            tab.title = name.trim().to_owned();
            tab.focus = true;
        }
        if accept || cancel {
            self.rename = None;
        }
    }

    fn draw_chrome_options(&mut self, ui: &mut egui::Ui) {
        let mut chrome = self.config.chrome;
        let mut changed = false;
        for (label, visible) in [
            ("菜单栏", &mut chrome.menu),
            ("工具栏", &mut chrome.toolbar),
            ("地址栏", &mut chrome.address),
            ("快捷栏", &mut chrome.shortcuts),
        ] {
            changed |= ui.checkbox(visible, label).changed();
        }
        if ui.button("显示全部栏").clicked() {
            chrome = Default::default();
            changed = true;
        }
        ui.weak("隐藏后可右键底部状态栏恢复；Ctrl+Shift+M 切换菜单栏。");
        if changed {
            self.submit(Action::ChromeVisibility(chrome), Pending::Update);
        }
    }

    fn configured_tab(&self, profile: Profile) -> Tab {
        let tab = Self::make_tab(profile);
        tab.terminal
            .lock()
            .unwrap()
            .set_history_limit(self.config.scrollback_lines);
        tab
    }

    fn make_tab(profile: Profile) -> Tab {
        Tab {
            jumpserver: None,
            local: false,
            font_size_override: None,
            id: Uuid::new_v4(),
            title: profile.name.clone(),
            profile,
            terminal: Arc::new(Mutex::new(Terminal::new(80, 24))),
            render_snapshot: None,
            connection: None,
            status: "未连接".into(),
            tunnel_statuses: Vec::new(),
            owns_tunnels: true,
            connected: false,
            finished: false,
            size: (80, 24),
            preedit: String::new(),
            search: String::new(),
            show_search: false,
            search_position: None,
            pending_paste: None,
            focus: true,
            pointer_in_terminal: false,
            pending_bottom: false,
            scroll_remainder: 0.0,
            host_prompt: None,
            transfer: transfer_view::Browser::default(),
            #[cfg(test)]
            test_sent: Vec::new(),
        }
    }

    fn submit(&mut self, action: Action, pending: Pending) -> bool {
        match self.store.submit(action) {
            Ok(id) => {
                self.pending.insert(id, pending);
                true
            }
            Err(error) => {
                self.message = error.to_string();
                false
            }
        }
    }

    fn profile_saving(&self) -> bool {
        self.pending
            .values()
            .any(|operation| matches!(operation, Pending::SaveProfile { .. }))
    }

    fn poll_store(&mut self, ctx: &egui::Context) {
        loop {
            let completion = match self.store.try_recv() {
                Ok(completion) => completion,
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    if !self.pending.is_empty() {
                        self.message = "后台配置服务已停止，未确认操作请重新检查".into();
                        self.pending.clear();
                    }
                    break;
                }
            };
            let Some(pending) = self.pending.remove(&completion.id) else {
                continue;
            };
            if matches!(&pending, Pending::AiKey { generation, .. } | Pending::AgentKey { generation, .. } if *generation != self.ai_generation)
            {
                continue;
            }
            match completion.result {
                Ok(Outcome::ImportedRsh(config, name)) => {
                    self.config = config;
                    self.show_sessions = true;
                    self.filter.clear();
                    self.message = format!("已导入会话「{name}」；凭据和主机信任需重新确认");
                }
                Ok(Outcome::ExportedRsh(path) | Outcome::ExportedXts(path)) => {
                    self.message = format!("会话已导出：{}", path.display());
                }
                Ok(Outcome::RepairedNames(config, report)) => {
                    self.config = config;
                    self.import_result = report.summary();
                    self.message = self.import_result.clone();
                    self.import_open = true;
                }
                Ok(Outcome::Imported(config, report)) => {
                    self.config = config;
                    self.import_result = report.summary();
                    self.message = self.import_result.clone();
                    self.import_open = true;
                    self.show_sessions = true;
                    self.filter.clear();
                }
                Ok(Outcome::Committed(config)) => {
                    let font_changed = self.config.ui_font != config.ui_font;
                    if self.config.scrollback_lines != config.scrollback_lines {
                        for tab in &mut self.tabs {
                            tab.terminal
                                .lock()
                                .unwrap()
                                .set_history_limit(config.scrollback_lines);
                        }
                    }
                    self.config = config;
                    if font_changed {
                        crate::install_fonts(ctx, self.config.ui_font);
                    }
                    apply_ui_appearance(ctx, &self.config);
                    crate::ui_appearance::apply_palette(ctx, &self.config);
                    ctx.set_theme(if self.config.light_theme {
                        egui::ThemePreference::Light
                    } else {
                        egui::ThemePreference::Dark
                    });
                    if let Pending::SaveProfile {
                        mut editor,
                        connect,
                    } = pending
                        && connect
                        && !self.close_when_idle
                    {
                        self.start_connection(
                            ctx,
                            editor.profile.clone(),
                            std::mem::take(&mut editor.secret),
                            editor.reconnect_tab,
                        );
                    }
                }
                Ok(Outcome::Secret(secret)) => {
                    if let Pending::AgentKey { request, .. } = pending {
                        if let Some(key) = secret {
                            self.start_agent_request(*request, key);
                        } else {
                            self.ai_error =
                                "此配置没有已保存的 API Key，请在 AI 设置中保存密钥".into();
                            self.restore_ai_question();
                            self.save_ai_history();
                        }
                        continue;
                    }
                    if let Pending::JumpToken(generation) = pending {
                        if generation == self.jumpserver.generation {
                            self.restore_jumpserver(ctx, secret);
                        }
                        continue;
                    }
                    if let Pending::AiKey {
                        ref profile,
                        generation,
                        ref prompt,
                    } = pending
                    {
                        if generation == self.ai_generation {
                            if let Some(key) = secret {
                                self.start_ai_request(ctx, profile.clone(), key, prompt.clone());
                            } else {
                                self.ai_error =
                                    "此配置没有已保存的 API Key，请在 AI 设置中保存密钥".into();
                                self.restore_ai_question();
                                self.save_ai_history();
                            }
                        }
                        continue;
                    }
                    let mut connect = false;
                    if let Pending::ReadSecret { generation } = pending
                        && let Some(editor) = self
                            .editor
                            .as_mut()
                            .filter(|editor| editor.generation == generation)
                    {
                        let missing = secret.is_none();
                        connect = editor.accept_secret(secret);
                        if missing {
                            self.message = "未找到保存的 RShell 加密凭据，请重新输入".into();
                        }
                    }
                    if connect && !self.close_when_idle {
                        let mut editor = self.editor.take().unwrap();
                        self.start_connection(
                            ctx,
                            editor.profile.clone(),
                            std::mem::take(&mut editor.secret),
                            editor.reconnect_tab,
                        );
                    }
                }
                Ok(Outcome::AiHistory(history)) => {
                    if matches!(pending, Pending::AiHistoryLoad) {
                        let history = if self.smoke
                            && std::env::args().any(|a| a == "--smoke-ai-long-draft")
                        {
                            let id = Uuid::new_v4();
                            rshell_core::ai::ChatHistory {
                                active_id: Some(id),
                                threads: vec![rshell_core::ai::ChatThread {
                                    id,
                                    title: "帮我分析选中的终端输出并生成查看当前目录文件的命令".into(),
                                    messages: vec![
                                        rshell_core::ai::ChatMessage { role: "user".into(), content: "帮我解释目录列表，并生成查看文件大小的命令。".into() },
                                        rshell_core::ai::ChatMessage { role: "assistant".into(), content: "可以使用下面的命令查看当前目录，包括隐藏文件，并以便于阅读的单位显示大小。\n\n```bash\nls -lah\n```\n\n请先检查执行目标，再确认是否运行。".into() },
                                    ],
                                }],
                            }
                        } else {
                            history
                        };
                        self.ai_thread_id = history.active_id;
                        self.ai_messages = history
                            .active_id
                            .and_then(|id| {
                                history
                                    .threads
                                    .iter()
                                    .find(|t| t.id == id)
                                    .map(|t| t.messages.clone())
                            })
                            .unwrap_or_default();
                        self.ai_history = history;
                        self.ai_history_loaded = true;
                    }
                }
                Ok(Outcome::AiHistorySaved | Outcome::JumpServerTokenSaved) => {}
                Err(error) => {
                    self.close_when_idle = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                    self.message = format!("后台保存或读取失败：{error}");
                    if matches!(pending, Pending::Import) {
                        self.import_result = self.message.clone();
                        self.import_open = true;
                    }
                    match pending {
                        Pending::SaveProfile { editor, .. } => {
                            self.editor = Some(*editor);
                        }
                        Pending::ReadSecret { generation } => {
                            if let Some(editor) = self
                                .editor
                                .as_mut()
                                .filter(|editor| editor.generation == generation)
                            {
                                editor.loading = false;
                            }
                        }
                        Pending::AiSave(editor) => self.ai_editor = Some(*editor),
                        Pending::AiKey { generation, .. }
                        | Pending::AgentKey { generation, .. }
                            if generation == self.ai_generation =>
                        {
                            self.ai_error = std::mem::take(&mut self.message);
                            self.restore_ai_question();
                            self.save_ai_history();
                        }
                        Pending::AiHistoryLoad => {
                            self.ai_history_loaded = true;
                            self.ai_error = std::mem::take(&mut self.message);
                        }
                        Pending::AiHistorySave => {
                            self.ai_error = std::mem::take(&mut self.message);
                        }
                        Pending::Folder(folder) => self.folder_editor = Some(folder),
                        Pending::HostEdit(mut editor) => {
                            editor.error = std::mem::take(&mut self.message);
                            self.host_editor = Some(*editor);
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    fn prepare_connection_tab(
        &mut self,
        profile: Profile,
        reconnect_tab: Option<Uuid>,
    ) -> Option<usize> {
        let mut tab = self.configured_tab(profile);
        if let Some(id) = reconnect_tab {
            let index = self.tabs.iter().position(|tab| tab.id == id)?;
            tab.id = id;
            tab.title = self.tabs[index].title.clone();
            tab.font_size_override = self.tabs[index].font_size_override;
            tab.focus = self.active == index;
            self.ai_command = None;
            self.tabs[index] = tab;
            Some(index)
        } else {
            self.tabs.push(tab);
            self.active = self.tabs.len() - 1;
            Some(self.active)
        }
    }

    fn start_local(&mut self, ctx: &egui::Context, reconnect_tab: Option<Uuid>) {
        let Some(index) = self.prepare_connection_tab(
            Profile {
                name: "PowerShell".into(),
                ..Default::default()
            },
            reconnect_tab,
        ) else {
            return;
        };
        let tab = &mut self.tabs[index];
        tab.local = true;
        tab.status = "正在启动本地终端…".into();
        tab.connection = Some(rshell_core::local::connect(
            self.runtime.handle(),
            TerminalAdapter {
                terminal: tab.terminal.clone(),
                ctx: ctx.clone(),
            },
        ));
        ctx.request_repaint();
    }

    fn switch_tab(&mut self, direction: isize, ctx: &egui::Context) {
        if self.terminal_layout != TerminalLayout::Single {
            self.pane_groups
                .sync(&self.tabs, &mut self.active, self.terminal_layout);
            self.pane_groups
                .switch(&self.tabs, &mut self.active, direction);
            if let Some(tab) = self.tabs.get_mut(self.active) {
                tab.focus = true;
            }
            ctx.request_repaint();
        } else if self.tabs.len() > 1 {
            self.active = adjacent_tab(self.active, self.tabs.len(), direction);
            self.tabs[self.active].focus = true;
            ctx.request_repaint();
        }
    }

    fn start_connection(
        &mut self,
        ctx: &egui::Context,
        profile: Profile,
        secret: String,
        reconnect_tab: Option<Uuid>,
    ) -> Option<&mut Tab> {
        let index = self.prepare_connection_tab(profile.clone(), reconnect_tab)?;
        let tab = &mut self.tabs[index];
        let adapter = TerminalAdapter {
            terminal: tab.terminal.clone(),
            ctx: ctx.clone(),
        };
        tab.connection = Some(if profile.protocol == Protocol::Serial {
            rshell_core::serial::connect(self.runtime.handle(), profile.serial.clone(), adapter)
        } else if profile.protocol == Protocol::Telnet {
            rshell_core::telnet::connect(self.runtime.handle(), profile, adapter)
        } else {
            ssh::connect(
                self.runtime.handle(),
                profile,
                secret,
                self.config.clone(),
                adapter,
            )
        });
        tab.status = "连接中".into();
        if reconnect_tab.is_none()
            && let Some(id) = self.startup_local_tab.take()
        {
            close_startup_local_tab(&mut self.tabs, &mut self.active, id);
            return self.tabs.get_mut(self.active);
        }
        self.tabs.get_mut(index)
    }

    fn poll(&mut self) {
        let mut recent = Vec::new();
        let mut jump_reconnects = Vec::new();
        for tab in &mut self.tabs {
            let mut remote_finished = false;
            if let Some(connection) = &mut tab.connection {
                while let Ok(event) = connection.events.try_recv() {
                    match event {
                        ssh::Event::TransferPrompt { upload, reply } => {
                            if tab.jumpserver.is_some() {
                                let _ = reply.send(None);
                                tab.status = "JumpServer 会话暂未启用 ZMODEM 传输".into();
                                continue;
                            }
                            tab.status = "等待选择 ZMODEM 文件 / 目录".into();
                            self.runtime.spawn(async move {
                                let dialog = rfd::AsyncFileDialog::new();
                                let selected = if upload {
                                    dialog.set_title("rz：选择要上传的文件").pick_file().await
                                } else {
                                    dialog
                                        .set_title("sz：选择接收目录（不覆盖同名文件）")
                                        .pick_folder()
                                        .await
                                };
                                let _ = reply.send(selected.map(|f| f.path().to_owned()));
                            });
                        }
                        ssh::Event::Status(status) => tab.status = status,
                        ssh::Event::Tunnels(statuses) => tab.tunnel_statuses = statuses,
                        ssh::Event::Connected => {
                            tab.connected = true;
                            tab.status = if let Some(reference) = &tab.jumpserver {
                                format!(
                                    "已连接 · {}@{} · 经 {}:{} 中转",
                                    reference.account,
                                    reference.asset.name,
                                    reference.settings.ssh_host,
                                    reference.settings.ssh_port
                                )
                            } else if tab.local {
                                "本地 Windows PowerShell · 初始提示符已就绪".into()
                            } else if tab.profile.protocol == Protocol::Telnet {
                                "已连接 · Telnet 明文".into()
                            } else if tab.profile.tunnel_only {
                                "已连接 · 仅隧道".into()
                            } else {
                                "已连接".into()
                            };
                            tab.focus = true;
                            tab.size = (0, 0);
                            if !tab.local && tab.jumpserver.is_none() {
                                recent.push(tab.profile.id);
                            }
                        }
                        ssh::Event::HostKey {
                            fingerprint,
                            public_key,
                            reply,
                            ..
                        } => {
                            tab.status = "正在保存首次连接的主机密钥…".into();
                            tab.host_prompt = Some(HostPrompt {
                                fingerprint,
                                public_key,
                                reply,
                            });
                        }
                        ssh::Event::Finished(result) => {
                            remote_finished = !tab.local && tab.connected;
                            if tab.connected
                                && result.is_err()
                                && let Some(mut reference) = tab.jumpserver.clone()
                            {
                                reference.reconnect_tab = Some(tab.id);
                                jump_reconnects.push(reference);
                            }
                            tab.connected = false;
                            tab.finished = true;
                            tab.host_prompt = None;
                            tab.status = match result {
                                Ok(()) => "已断开".into(),
                                Err(error) => error,
                            };
                        }
                    }
                }
            }
            if remote_finished && self.config.session_options.clear_scrollback_on_close {
                *tab.terminal.lock().unwrap() =
                    Terminal::with_history(tab.size.0, tab.size.1, self.config.scrollback_lines);
                tab.search_position = None;
            }
            if tab
                .host_prompt
                .as_ref()
                .is_some_and(|p| p.reply.is_closed())
            {
                tab.host_prompt = None;
            }
        }
        for reference in jump_reconnects {
            self.queue_jumpserver_reconnect(reference);
        }
        if !recent.is_empty() {
            self.submit(Action::Recent(recent), Pending::Update);
        }
    }

    fn folder_tree(&mut self, ui: &mut egui::Ui, parent: Option<Uuid>) {
        let filter = self.filter.to_lowercase();
        let mut folders: Vec<_> = self
            .config
            .folders
            .iter()
            .filter(|f| f.parent_id == parent)
            .cloned()
            .collect();
        folders.sort_by(|a, b| a.name.cmp(&b.name));
        for folder in folders {
            if !folder_matches(&self.config, folder.id, &filter) {
                continue;
            }
            let header = egui::CollapsingHeader::new(
                RichText::new(&folder.name).size(15.0 * self.sessions_zoom),
            )
            .icon(folder_icon)
            .id_salt((folder.id, !filter.is_empty()))
            .default_open(true)
            .open(if filter.is_empty() { None } else { Some(true) })
            .show(ui, |ui| self.folder_tree(ui, Some(folder.id)));
            let handle = egui::Rect::from_min_max(
                egui::pos2(
                    header.header_response.rect.right() - 24.0 * self.sessions_zoom,
                    header.header_response.rect.top(),
                ),
                header.header_response.rect.right_bottom(),
            );
            let drag = ui.interact(
                handle,
                ui.scope_id().with(("folder_drag", folder.id)),
                egui::Sense::drag(),
            );
            ui.painter().text(
                handle.center(),
                egui::Align2::CENTER_CENTER,
                "⋮⋮",
                egui::FontId::proportional(13.0 * self.sessions_zoom),
                ui.visuals().weak_text_color(),
            );
            drag.on_hover_text("拖动文件夹到其他文件夹或所有会话")
                .dnd_set_drag_payload(SessionDrag::Folder(folder.id));
            if let Some(action) =
                folder_drop(ui, &header.header_response, &self.config, Some(folder.id))
            {
                self.submit(action, Pending::Update);
            }
            header.header_response.context_menu(|ui| {
                if ui.button("新建会话").clicked() {
                    self.open_profile_editor(
                        Profile {
                            folder_id: Some(folder.id),
                            ..self.new_profile()
                        },
                        false,
                        None,
                    );
                    ui.close();
                }
                if ui.button("新建子文件夹").clicked() {
                    self.folder_editor = Some(Folder {
                        id: Uuid::new_v4(),
                        name: String::new(),
                        parent_id: Some(folder.id),
                    });
                    ui.close();
                }
                if ui.button("重命名 / 移动文件夹").clicked() {
                    self.folder_editor = Some(folder.clone());
                    ui.close();
                }
                let empty = !self
                    .config
                    .folders
                    .iter()
                    .any(|f| f.parent_id == Some(folder.id))
                    && !self
                        .config
                        .profiles
                        .iter()
                        .any(|p| p.folder_id == Some(folder.id));
                if ui
                    .add_enabled(empty, egui::Button::new("删除空文件夹"))
                    .on_disabled_hover_text("请先移动会话和子文件夹")
                    .clicked()
                {
                    self.submit(Action::DeleteFolder(folder.id), Pending::Update);
                    ui.close();
                }
            });
        }
        let mut profiles: Vec<_> = self
            .config
            .profiles
            .iter()
            .filter(|p| p.folder_id == parent && profile_matches(&self.config, p, &filter))
            .cloned()
            .collect();
        profiles.sort_by(|a, b| a.name.cmp(&b.name));
        for profile in profiles {
            let selected = self
                .tabs
                .get(self.active)
                .is_some_and(|tab| tab.profile.id == profile.id);
            let response = session_row(ui, &profile.name, selected, self.sessions_zoom)
                .on_hover_text(format!(
                    "{} · {}",
                    profile.endpoint(),
                    self.config.folder_path(profile.folder_id)
                ));
            response.dnd_set_drag_payload(SessionDrag::Profile(profile.id));
            if response.clicked() {
                self.open_profile(profile.clone());
            }
            response.context_menu(|ui| {
                let pinned = self.config.shortcut_profiles.contains(&profile.id);
                if ui
                    .button(if pinned {
                        "从快捷栏移除"
                    } else {
                        "添加到快捷栏"
                    })
                    .clicked()
                {
                    self.submit(
                        Action::SetShortcut {
                            id: profile.id,
                            enabled: !pinned,
                        },
                        Pending::Update,
                    );
                    ui.close();
                }
                if ui.button("编辑会话").clicked() {
                    self.open_profile_editor(profile.clone(), false, None);
                    ui.close();
                }
                if ui.button("复制会话").clicked() {
                    let mut copy = profile.clone();
                    copy.id = Uuid::new_v4();
                    copy.name.push_str(" 副本");
                    copy.remember_secret = false;
                    self.open_profile_editor(copy, false, None);
                    ui.close();
                }
                if ui.button("导出全部会话 (.rsh)…").clicked() {
                    self.choose_session_file(ui.ctx(), Some(false));
                    ui.close();
                }
                if ui
                    .add_enabled(
                        self.config
                            .profiles
                            .iter()
                            .any(|p| p.protocol != Protocol::Serial),
                        egui::Button::new("导出全部给 Xshell (.xts)…"),
                    )
                    .clicked()
                {
                    self.choose_session_file(ui.ctx(), Some(true));
                    ui.close();
                }
                ui.menu_button("移动到文件夹", |ui| {
                    let mut destination = profile.folder_id;
                    folder_picker(
                        ui,
                        &self.config,
                        &mut destination,
                        ("move_profile", profile.id),
                    );
                    if destination != profile.folder_id {
                        self.submit(
                            Action::MoveProfile {
                                id: profile.id,
                                folder_id: destination,
                            },
                            Pending::Update,
                        );
                        ui.close();
                    }
                });
                if ui.button("删除会话配置").clicked() {
                    self.delete = Some(profile.id);
                    ui.close();
                }
            });
        }
    }

    fn folder_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut folder) = self.folder_editor.take() else {
            return;
        };
        let mut open = true;
        let mut save = false;
        let mut cancel = false;
        egui::Window::new("文件夹设置")
            .id(egui::Id::unique("folder_editor"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("文件夹名称");
                ui.text_edit_singleline(&mut folder.name);
                ui.label("父文件夹");
                folder_picker(ui, &self.config, &mut folder.parent_id, "parent_folder");
                ui.horizontal(|ui| {
                    save = ui.button("保存").clicked();
                    cancel = ui.button("取消").clicked();
                });
            });
        if save {
            if !self.submit(
                Action::SaveFolder(folder.clone()),
                Pending::Folder(folder.clone()),
            ) {
                self.folder_editor = Some(folder);
            }
        } else if open && !cancel {
            self.folder_editor = Some(folder);
        }
    }

    fn open_profile(&mut self, profile: Profile) {
        self.open_profile_editor(profile, true, None);
    }

    fn open_profile_editor(
        &mut self,
        profile: Profile,
        connect_after_load: bool,
        reconnect_tab: Option<Uuid>,
    ) {
        if self.profile_saving() {
            self.message = "正在保存会话，请等待完成".into();
            return;
        }
        let mut editor = Editor::new(profile);
        editor.connect_after_load = connect_after_load;
        editor.reconnect_tab = reconnect_tab;
        if editor.profile.protocol == Protocol::Ssh
            && editor.profile.remember_secret
            && self
                .config
                .profiles
                .iter()
                .any(|p| p.id == editor.profile.id)
        {
            editor.loading = self.submit(
                Action::ReadSecret(editor.profile.id),
                Pending::ReadSecret {
                    generation: editor.generation,
                },
            );
        }
        self.editor = Some(editor);
    }

    fn editor(&mut self, ctx: &egui::Context) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        if let Some(picker) = &editor.picker {
            match picker.try_recv() {
                Ok(path) => {
                    if let Some(path) = path {
                        editor.profile.key_path = path.to_string_lossy().into_owned();
                    }
                    editor.picker = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => editor.picker = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(scan) = &editor.port_scan {
            match scan.try_recv() {
                Ok(result) => {
                    match result {
                        Ok(ports) => {
                            editor.serial_ports = ports;
                            editor.port_error.clear();
                        }
                        Err(e) => editor.port_error = e,
                    }
                    editor.port_scan = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    editor.port_scan = None;
                    editor.port_error = "串口枚举任务已结束".into();
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        let mut action = 0;
        let mut open = true;
        egui::Window::new("会话设置")
            .id(egui::Id::unique("session_editor"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .frame(
                egui::Frame::window(&ctx.style_of(if self.config.light_theme {
                    egui::Theme::Light
                } else {
                    egui::Theme::Dark
                }))
                .inner_margin(20.0),
            )
            .default_width(850.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                settings_view::dialog_style(ui, self.config.ui_font_size);
                if editor.loading {
                    ui.spinner();
                    ui.label("正在后台读取 RShell 加密凭据…");
                    if ui.button("取消").clicked() {
                        action = 4;
                    }
                    return;
                }
                ui.horizontal_top(|ui| {
                    ui.allocate_ui_with_layout(
                        egui::vec2(150.0, 470.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.heading("连接方式");
                            ui.add_space(16.0);
                            if ui.selectable_value(&mut editor.profile.protocol, Protocol::Ssh, "SSH").changed()
                                && editor.profile.port == 23 { editor.profile.port = 22; }
                            ui.add_space(8.0);
                            if editor.profile.protocol == Protocol::Ssh {
                                ui.selectable_value(&mut editor.tunnel_settings, false, "登录设置");
                                ui.selectable_value(&mut editor.tunnel_settings, true, "SSH 隧道");
                                ui.add_space(8.0);
                            }
                            if ui.selectable_value(&mut editor.profile.protocol, Protocol::Telnet, "Telnet（明文）").changed()
                                && editor.profile.port == 22 { editor.profile.port = 23; }
                            ui.add_space(8.0);
                            ui.selectable_value(
                                &mut editor.profile.protocol,
                                Protocol::Serial,
                                "串口 (Serial)",
                            );
                        },
                    );
                    let (divider, _) =
                        ui.allocate_exact_size(egui::vec2(1.0, 470.0), egui::Sense::hover());
                    ui.painter().vline(
                        divider.center().x,
                        divider.y_range(),
                        ui.visuals().widgets.noninteractive.bg_stroke,
                    );
                    ui.add_space(10.0);
                    ui.vertical(|ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("session_fields")
                            .max_height(540.0)
                            .show(ui, |ui| {
                                if editor.tunnel_settings && editor.profile.protocol == Protocol::Ssh {
                                    crate::tunnel_view::settings(ui, &mut editor.profile);
                                    return;
                                }
                                ui.label(RichText::new("连接设置").size(22.0).strong());
                                ui.label(
                                    RichText::new("选择 SSH、Telnet 或串口，连接后将在新标签打开。").weak(),
                                );
                                ui.add_space(18.0);
                                egui::Grid::new("profile_fields")
                                    .num_columns(2)
                                    .spacing([24.0, 16.0])
                                    .show(ui, |ui| {
                                        ui.label("名称");
                                        ui.text_edit_singleline(&mut editor.profile.name);
                                        ui.end_row();
                                        ui.label("文件夹");
                                        folder_picker(
                                            ui,
                                            &self.config,
                                            &mut editor.profile.folder_id,
                                            "profile_folder",
                                        );
                                        ui.end_row();
                                        if editor.profile.protocol == Protocol::Serial {
                                            editor.secret.zeroize();
                                            editor.profile.remember_secret = false;
                                            editor.loaded_identity = None;
                                            serial_fields(ui, editor, ctx);
                                            return;
                                        }
                                        ui.label("主机 / IP");
                                        ui.text_edit_singleline(&mut editor.profile.host);
                                        ui.end_row();
                                        ui.label("端口");
                                        ui.add(
                                            egui::DragValue::new(&mut editor.profile.port)
                                                .range(1..=65535),
                                        );
                                        ui.end_row();
                                        ui.label("用户名");
                                        ui.text_edit_singleline(&mut editor.profile.username);
                                        ui.end_row();
                                        if editor.profile.protocol == Protocol::Telnet {
                                            editor.secret.zeroize();
                                            editor.profile.remember_secret = false;
                                            editor.loaded_identity = None;
                                            ui.label("登录说明");
                                            ui.label("Telnet 不加密；用户名仅作备注，账号和密码请在终端手动输入。");
                                            ui.end_row();
                                            return;
                                        }
                                        ui.label("认证");
                                        ui.horizontal(|ui| {
                                            ui.selectable_value(
                                                &mut editor.profile.auth,
                                                AuthMethod::Password,
                                                "密码",
                                            );
                                            ui.selectable_value(
                                                &mut editor.profile.auth,
                                                AuthMethod::PrivateKey,
                                                "私钥",
                                            );
                                        });
                                        ui.end_row();
                                        if editor.profile.auth == AuthMethod::PrivateKey {
                                            ui.label("私钥路径");
                                            ui.horizontal(|ui| {
                                                ui.add(
                                                    egui::TextEdit::singleline(
                                                        &mut editor.profile.key_path,
                                                    )
                                                    .desired_width(280.0)
                                                    .hint_text("选择私钥文件"),
                                                );
                                                if ui
                                                    .add_enabled(
                                                        editor.picker.is_none(),
                                                        egui::Button::new("浏览…"),
                                                    )
                                                    .clicked()
                                                {
                                                    let (tx, rx) = std::sync::mpsc::channel();
                                                    let notify = ctx.clone();
                                                    std::thread::spawn(move || {
                                                        let path = rfd::FileDialog::new()
                                                            .set_title("选择 SSH 私钥")
                                                            .pick_file();
                                                        let _ = tx.send(path);
                                                        notify.request_repaint();
                                                    });
                                                    editor.picker = Some(rx);
                                                }
                                            });
                                            ui.end_row();
                                        }
                                        ui.label(if editor.profile.auth == AuthMethod::Password {
                                            "密码"
                                        } else {
                                            "私钥口令"
                                        });
                                        ui.horizontal(|ui| {
                                            let response = ui.add(
                                                egui::TextEdit::singleline(&mut editor.secret)
                                                    .desired_width(280.0)
                                                    .password(!editor.show_secret)
                                                    .hint_text(
                                                        if editor.profile.auth
                                                            == AuthMethod::Password
                                                        {
                                                            "输入登录密码"
                                                        } else {
                                                            "未加密的私钥可留空"
                                                        },
                                                    ),
                                            );
                                            if editor.focus_secret
                                                && !editor.profile.host.is_empty()
                                            {
                                                response.request_focus();
                                                editor.focus_secret = false;
                                            }
                                            if response.lost_focus()
                                                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                                            {
                                                action = editor.enter_action();
                                            }
                                            ui.toggle_value(&mut editor.show_secret, "显示");
                                            if ui
                                                .add_enabled(
                                                    !editor.secret.is_empty(),
                                                    egui::Button::new("复制"),
                                                )
                                                .on_hover_text("复制密码或私钥口令到剪贴板")
                                                .clicked()
                                            {
                                                ctx.copy_text(editor.secret.clone());
                                            }
                                        });
                                        ui.end_row();
                                    });
                                if editor.profile.protocol == Protocol::Ssh {
                                    ui.add_space(12.0);
                                    ui.checkbox(
                                        &mut editor.profile.remember_secret,
                                        "记住密码 / 口令，下次连接自动登录",
                                    );
                                    ui.label(
                    RichText::new(
                        "勾选后按 Enter 或「保存并连接」保存到 RShell 加密文件；「仅连接」不保存。",
                    )
                    .small()
                    .weak(),
                );
                                }
                            });
                    });
                });
                ui.add_space(14.0);
                ui.separator();
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("保存会话").clicked() {
                        action = 1;
                    }
                    if ui.button("保存并连接").clicked() {
                        action = 2;
                    }
                    let auto_save = self.config.session_options.auto_save_session
                        && self.config.profiles.iter().any(|profile| profile.id == editor.profile.id);
                    if ui.button(if auto_save { "连接（自动保存）" } else { "仅连接" }).clicked() {
                        action = 3;
                    }
                    if ui.button("取消").clicked() {
                        action = 4;
                    }
                });
            });
        if editor
            .loaded_identity
            .as_ref()
            .is_some_and(|identity| *identity != editor.identity())
        {
            editor.secret.zeroize();
            editor.loaded_identity = None;
        }
        if !open || action == 4 {
            self.editor = None;
            return;
        }
        if action == 0 {
            return;
        }
        let editor = self.editor.as_ref().unwrap();
        if editor.loading {
            return;
        }
        if let Err(error) = editor.profile.validate() {
            self.message = error.to_string();
            return;
        }
        let mut editor = self.editor.take().unwrap();
        let auto_save = action == 3
            && self.config.session_options.auto_save_session
            && self
                .config
                .profiles
                .iter()
                .any(|profile| profile.id == editor.profile.id);
        if action <= 2 || auto_save {
            let action_to_save = if auto_save {
                Action::SaveProfileMetadata(editor.profile.clone())
            } else {
                Action::SaveProfile {
                    profile: editor.profile.clone(),
                    secret: Zeroizing::new(editor.secret.clone()),
                }
            };
            match self.store.submit(action_to_save) {
                Ok(id) => {
                    self.pending.insert(
                        id,
                        Pending::SaveProfile {
                            editor: Box::new(editor),
                            connect: action == 2 || auto_save,
                        },
                    );
                }
                Err(error) => {
                    self.message = error.to_string();
                    self.editor = Some(editor);
                }
            }
        } else {
            self.start_connection(
                ctx,
                editor.profile.clone(),
                std::mem::take(&mut editor.secret),
                editor.reconnect_tab,
            );
        }
    }

    fn host_editor(&mut self, ctx: &egui::Context) {
        let Some(editor) = &mut self.host_editor else {
            return;
        };
        let mut action = 0;
        egui::Modal::new(egui::Id::unique("host_key_editor")).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.heading("独立核验主机密钥");
            ui.strong(format!("{}:{}", editor.original.host, editor.original.port));
            ui.label("当前信任的指纹");
            ui.monospace(&editor.original.fingerprint);
            ui.separator();
            ui.label("从服务器管理员或可信管理渠道取得主机公钥与 SHA-256 指纹后再更新。密钥变化的原连接仍然保持阻断，更新后需重新连接。");
            ui.label("主机公钥（OpenSSH 格式）");
            ui.add(egui::TextEdit::multiline(&mut editor.public_key).desired_width(f32::INFINITY).desired_rows(3));
            ui.label("通过可信渠道核验的指纹");
            ui.add(egui::TextEdit::singleline(&mut editor.fingerprint).desired_width(f32::INFINITY).hint_text("SHA256:…"));
            ui.checkbox(&mut editor.verified, "我已核验此次信任变更");
            if !editor.error.is_empty() { ui.colored_label(Color32::LIGHT_RED, &editor.error); }
            ui.label("移除会删除此主机和端口的信任记录，下次连接将自动记录新密钥；不会断开已有连接。");
            ui.horizontal(|ui| {
                if ui.button("取消").clicked() { action = 3; }
                if ui.add_enabled(editor.verified, egui::Button::new("核验并更新")).clicked() { action = 1; }
                if ui.add_enabled(editor.verified, egui::Button::new("移除信任记录")).clicked() { action = 2; }
            });
        });
        if action == 3 {
            self.host_editor = None;
            return;
        }
        let request = match action {
            1 => match KnownHost::verified(
                editor.original.host.clone(),
                editor.original.port,
                &editor.public_key,
                &editor.fingerprint,
            ) {
                Ok(replacement) => Action::ReplaceHost {
                    expected: editor.original.clone(),
                    replacement,
                },
                Err(error) => {
                    editor.error = error.to_string();
                    return;
                }
            },
            2 => Action::RemoveHost(editor.original.clone()),
            _ => return,
        };
        match self.store.submit(request) {
            Ok(id) => {
                self.pending.insert(
                    id,
                    Pending::HostEdit(Box::new(self.host_editor.take().unwrap())),
                );
            }
            Err(error) => self.host_editor.as_mut().unwrap().error = error.to_string(),
        }
    }

    fn trust_new_hosts(&mut self) {
        for index in 0..self.tabs.len() {
            let Some(prompt) = self.tabs[index].host_prompt.take() else {
                continue;
            };
            let profile = &self.tabs[index].profile;
            let host = match KnownHost::verified(
                profile.host.clone(),
                profile.port,
                &prompt.public_key,
                &prompt.fingerprint,
            ) {
                Ok(host) => host,
                Err(error) => {
                    self.message = error.to_string();
                    let _ = prompt.reply.send(false);
                    continue;
                }
            };
            self.submit(
                Action::TrustHost {
                    host,
                    reply: prompt.reply,
                },
                Pending::Host,
            );
        }
    }
}

// Native clipboard interception is disabled so terminal Ctrl+V reaches Vim,
// including when the clipboard is empty. Restore standard shortcuts in text fields.
fn text_edit_clipboard_shortcuts(ctx: &egui::Context, input: &mut egui::RawInput) {
    let text_focus = ctx
        .memory(|memory| memory.focused())
        .is_some_and(|id| egui::text_edit::TextEditState::load(ctx, id).is_some());
    if !text_focus {
        return;
    }
    input.events.retain_mut(|event| {
        if let egui::Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } = event
        {
            let ctrl_only = modifiers.ctrl && !modifiers.alt && !modifiers.shift;
            let shift_only = modifiers.shift && !modifiers.alt && !modifiers.ctrl;
            if *key == egui::Key::Copy
                || (ctrl_only && matches!(key, egui::Key::C | egui::Key::Insert))
            {
                *event = egui::Event::Copy;
            } else if *key == egui::Key::Cut
                || (ctrl_only && *key == egui::Key::X)
                || (shift_only && *key == egui::Key::Delete)
            {
                *event = egui::Event::Cut;
            } else if *key == egui::Key::Paste
                || (ctrl_only && *key == egui::Key::V)
                || (shift_only && *key == egui::Key::Insert)
            {
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
                return false;
            }
        }
        true
    });
}

impl eframe::App for App {
    fn raw_input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        text_edit_clipboard_shortcuts(ctx, input);
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_session_file_picker();
        self.poll_store(ctx);
        self.poll_ai_request();
        self.updates.poll();
        if self.updates.exit_for_install {
            self.exit_approved = true;
            self.close_when_idle = true;
        }
        self.poll();
        if ctx.input(|input| input.viewport().close_requested()) {
            if !self.exit_approved
                && !self.smoke
                && self.config.session_options.confirm_exit
                && self.tabs.iter().any(|tab| tab.connected && !tab.local)
            {
                self.confirm_exit_open = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            } else if !self.pending.is_empty() {
                self.close_when_idle = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
        }
        if self.close_when_idle && self.pending.is_empty() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint_after(Duration::from_millis(100));
        if self.smoke && self.started.elapsed() > Duration::from_secs(6) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if consume_menu_visibility_shortcut(&ctx) {
            let mut chrome = self.config.chrome;
            chrome.menu = !chrome.menu;
            self.submit(Action::ChromeVisibility(chrome), Pending::Update);
        }
        self.poll_jumpserver(&ctx);
        self.jumpserver_dialog(&ctx);
        clear_selection_on_primary_click(&ctx, self.tabs.get_mut(self.active));
        self.import_dialog(&ctx);
        if !self.screenshot_requested && self.started.elapsed() > Duration::from_secs(4) {
            if let Some(path) = std::env::var_os("RSHELL_SCREENSHOT") {
                ctx.request_screenshot(move |image| {
                    let bytes: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
                    let result = image::save_buffer(
                        PathBuf::from(path),
                        &bytes,
                        image.width() as u32,
                        image.height() as u32,
                        image::ColorType::Rgba8,
                    );
                    if let Err(error) = result {
                        eprintln!("Screenshot failed: {error}");
                    }
                });
            }
            self.screenshot_requested = true;
        }
        for tab in &mut self.tabs {
            tab.transfer.show(
                &ctx,
                self.runtime.handle(),
                tab.connection.as_ref(),
                tab.id,
                &tab.title,
            );
        }
        if self.terminal_layout != TerminalLayout::Single {
            self.pane_groups
                .sync(&self.tabs, &mut self.active, self.terminal_layout);
        } else if self.active >= self.tabs.len() {
            self.active = 0;
        }
        let mut allow_terminal_input = !self.updates.open
            && !self.about_open
            && !self.jumpserver.modal_open()
            && !self.import_open
            && !self.confirm_exit_open
            && self.pending_close.is_none()
            && self.pending_disconnect.is_none()
            && self.rename.is_none()
            && self.folder_editor.is_none()
            && self.editor.is_none()
            && self.host_editor.is_none()
            && self.ai_editor.is_none()
            && self.ai_delete.is_none()
            && !self.keys.open
            && !self.keys.hosts_open
            && !self.settings
            && self.delete.is_none()
            && self.message.is_empty()
            && !self.tabs.iter().any(|tab| tab.host_prompt.is_some());
        if allow_terminal_input {
            if consume_shortcut(&ctx, &self.config.shortcuts.new_local) {
                self.start_local(&ctx, None);
                allow_terminal_input = false;
            }
            if consume_shortcut(&ctx, &self.config.shortcuts.close_tab) {
                self.chrome_action(8, &ctx);
                allow_terminal_input = false;
            }
            if consume_shortcut(&ctx, &self.config.shortcuts.previous_tab)
                || consume_shortcut(&ctx, &self.config.shortcuts.previous_tab_alt)
                || ctx.input_mut(|i| {
                    i.consume_key(
                        egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                        egui::Key::Tab,
                    )
                })
            {
                self.switch_tab(-1, &ctx);
                allow_terminal_input = false;
            }
            if consume_shortcut(&ctx, &self.config.shortcuts.next_tab)
                || consume_shortcut(&ctx, &self.config.shortcuts.next_tab_alt)
                || ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Tab))
            {
                self.switch_tab(1, &ctx);
                allow_terminal_input = false;
            }
        }
        if self.config.chrome.any() {
            egui::Panel::top("header").show(ui, |ui| {
                let mut action = None;
                let mut recent_profile = None;
                let mut export_profile = None;
                if self.config.chrome.menu {
                    egui::MenuBar::new().ui(ui, |ui| {
                        for (title, entries) in [
                            (
                                "文件(F)",
                                vec![
                                    ("新建 SSH / Telnet / 串口会话…", 0),
                                    ("新建 SSH 隧道连接…", 16),
                                    ("本地 PowerShell", 1),
                                    ("关闭当前标签", 8),
                                ],
                            ),
                            ("编辑(E)", vec![("复制选区", 4), ("查找…", 5)]),
                            (
                                "查看(V)",
                                vec![
                                    ("显示 / 隐藏会话管理器", 7),
                                    ("显示 / 隐藏 AI 对话", 11),
                                    ("回到底部", 6),
                                ],
                            ),
                            (
                                "工具(T)",
                                vec![("重新连接", 2), ("断开连接", 3), ("SFTP 文件传输…", 12)],
                            ),
                            (
                                "选项(O)",
                                vec![("设置…", 9), ("JumpServer 堡垒机配置…", 15)],
                            ),
                        ] {
                            ui.menu_button(title, |ui| {
                                if title == "查看(V)" {
                                    ui.menu_button("界面栏显示", |ui| {
                                        self.draw_chrome_options(ui)
                                    });
                                    ui.separator();
                                }
                                if title == "工具(T)" {
                                    if ui.button("主机密钥管理器(H)…").clicked() {
                                        self.keys.hosts_open = true;
                                        ui.close();
                                    }
                                    if ui.button("用户密钥管理器(U)…").clicked() {
                                        self.keys.open = true;
                                        self.refresh_user_keys(ctx.clone());
                                        ui.close();
                                    }
                                    ui.separator();
                                }
                                for (label, id) in entries {
                                    if ui.button(label).clicked() {
                                        action = Some(id);
                                        ui.close();
                                    }
                                    if title == "文件(F)" && id == 0 {
                                        ui.menu_button("导入/导出", |ui| {
                                            for (label, action_id) in [
                                                ("导入 Xshell 会话 (.xts)…", 13),
                                                ("导入 RShell 会话 (.rsh)…", 14),
                                            ] {
                                                if ui.button(label).clicked() {
                                                    action = Some(action_id);
                                                    ui.close();
                                                }
                                            }
                                            ui.separator();
                                            for (label, xshell) in [
                                                ("导出全部 RShell 会话 (.rsh)…", false),
                                                ("导出全部给 Xshell (.xts)…", true),
                                            ] {
                                                let available =
                                                    self.config.profiles.iter().any(|profile| {
                                                        !xshell
                                                            || profile.protocol != Protocol::Serial
                                                    });
                                                if ui
                                                    .add_enabled(
                                                        available,
                                                        egui::Button::new(label),
                                                    )
                                                    .clicked()
                                                {
                                                    export_profile = Some(xshell);
                                                    ui.close();
                                                }
                                            }
                                        });
                                    }
                                }
                                if title == "文件(F)" {
                                    ui.menu_button("最近连接", |ui| {
                                        let recent: Vec<_> = self
                                            .config
                                            .recent
                                            .iter()
                                            .filter_map(|id| {
                                                self.config.profiles.iter().find(|p| p.id == *id)
                                            })
                                            .collect();
                                        if recent.is_empty() && self.jumpserver.recent.is_empty() {
                                            ui.add_enabled(
                                                false,
                                                egui::Button::new("暂无最近连接"),
                                            );
                                        }
                                        for profile in recent {
                                            if ui.button(&profile.name).clicked() {
                                                recent_profile = Some(profile.clone());
                                                ui.close();
                                            }
                                        }
                                        if !self.jumpserver.recent.is_empty() {
                                            ui.separator();
                                            let mut selected = None;
                                            for asset in &self.jumpserver.recent {
                                                if ui
                                                    .button(format!("JumpServer · {}", asset.name))
                                                    .clicked()
                                                {
                                                    selected = Some(asset.clone());
                                                    ui.close();
                                                }
                                            }
                                            if let Some(asset) = selected {
                                                self.open_jump_asset(&ctx, asset);
                                            }
                                        }
                                    });
                                }
                            });
                        }
                        ui.menu_button("窗口(W)", |ui| {
                            for (layout, label) in [
                                (TerminalLayout::Single, "单终端（标签切换）"),
                                (TerminalLayout::Columns, "左右分屏"),
                                (TerminalLayout::Rows, "上下分屏"),
                                (TerminalLayout::Grid, "网格分屏"),
                            ] {
                                if ui
                                    .selectable_value(&mut self.terminal_layout, layout, label)
                                    .clicked()
                                {
                                    self.ai_command = None;
                                    if let Some(tab) = self.tabs.get_mut(self.active) {
                                        tab.focus = true;
                                    }
                                    ui.close();
                                }
                            }
                            ui.separator();

                            let mut selected_tab = None;
                            for (index, tab) in self.tabs.iter().enumerate() {
                                if ui
                                    .selectable_label(index == self.active, &tab.title)
                                    .clicked()
                                {
                                    selected_tab = Some(index);
                                    ui.close();
                                }
                            }
                            if let Some(index) = selected_tab {
                                self.active = index;
                                self.tabs[index].focus = true;
                            }
                        });
                        ui.menu_button("帮助(H)", |ui| {
                            if ui.button("关于 RShell…").clicked() {
                                self.about_open = true;
                                ui.close();
                            }
                            if ui.button("检查更新…").clicked() {
                                self.check_updates(ctx.clone());
                                ui.close();
                            }
                        });
                    });
                }
                if self.config.chrome.toolbar {
                    ui.horizontal(|ui| {
                        for (tip, id) in [
                            ("新建 SSH / Telnet / 串口会话", 0),
                            ("本地 PowerShell", 1),
                            ("重新连接", 2),
                            ("断开连接", 3),
                            ("复制选区", 4),
                            ("回到底部", 6),
                            ("会话管理器", 7),
                            ("设置", 9),
                        ] {
                            if toolbar_icon(ui, id, 1.0).on_hover_text(tip).clicked() {
                                action = Some(id);
                            }
                            if id == 1 || id == 3 || id == 6 {
                                ui.separator();
                            }
                        }
                        if ui.button("SFTP").clicked() {
                            action = Some(12);
                        }
                        if ui.selectable_label(self.show_ai_chat, "AI 对话").clicked() {
                            self.show_ai_chat = !self.show_ai_chat;
                        }
                        if !self.pending.is_empty() {
                            ui.spinner();
                            ui.label("正在处理本地配置…");
                        }
                    });
                }
                if self.config.chrome.address {
                    ui.horizontal(|ui| {
                        ui.label("◇");
                        let address = ui.add_sized(
                            [ui.available_width() - 48.0, 23.0],
                            egui::TextEdit::singleline(&mut self.address)
                                .hint_text("主机、IP 地址或会话名称"),
                        );
                        let enter =
                            address.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if (ui.button("连接").clicked() || enter) && !self.address.trim().is_empty()
                        {
                            let value = self.address.trim();
                            let profile = self
                                .config
                                .profiles
                                .iter()
                                .find(|p| p.name == value || p.host == value)
                                .cloned()
                                .unwrap_or_else(|| Profile {
                                    name: value.into(),
                                    host: value.into(),
                                    ..self.new_profile()
                                });
                            self.open_profile(profile);
                        }
                    });
                }
                if self.config.chrome.shortcuts {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 3.0;
                        ui.menu_button("+", |ui| {
                            if ui.button("新建会话").clicked() {
                                action = Some(0);
                                ui.close();
                            }
                            let available: Vec<_> = self
                                .config
                                .profiles
                                .iter()
                                .filter(|profile| {
                                    !self.config.shortcut_profiles.contains(&profile.id)
                                })
                                .cloned()
                                .collect();
                            if !available.is_empty() {
                                ui.separator();
                                ui.label("添加会话到快捷栏");
                                egui::ScrollArea::vertical()
                                    .max_height(320.0)
                                    .show(ui, |ui| {
                                        for profile in available {
                                            if ui
                                                .button(&profile.name)
                                                .on_hover_text(profile.endpoint())
                                                .clicked()
                                            {
                                                self.submit(
                                                    Action::SetShortcut {
                                                        id: profile.id,
                                                        enabled: true,
                                                    },
                                                    Pending::Update,
                                                );
                                                ui.close();
                                            }
                                        }
                                    });
                            }
                        });
                        let strip_width = (ui.available_width() - 52.0).max(64.0);
                        if ui
                            .add_enabled(
                                self.shortcut_offset > 1.0,
                                egui::Button::new("◀").min_size(egui::vec2(22.0, 22.0)),
                            )
                            .on_hover_text("快捷栏向左翻页")
                            .clicked()
                        {
                            self.shortcut_offset = (self.shortcut_offset - strip_width).max(0.0);
                        }
                        let shortcuts = egui::ScrollArea::horizontal()
                            .id_salt("session_shortcuts")
                            .max_width(strip_width)
                            .auto_shrink([false, true])
                            .scroll_bar_visibility(
                                egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                            )
                            .horizontal_scroll_offset(self.shortcut_offset)
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 12.0;
                                    let shortcuts: Vec<_> = self
                                        .config
                                        .shortcut_profiles
                                        .iter()
                                        .filter_map(|id| {
                                            self.config
                                                .profiles
                                                .iter()
                                                .find(|profile| profile.id == *id)
                                        })
                                        .cloned()
                                        .collect();
                                    for profile in shortcuts {
                                        let response = ui
                                            .add(
                                                egui::Button::new(format!("↗ {}", profile.name))
                                                    .frame(false),
                                            )
                                            .on_hover_text(profile.endpoint());
                                        response.context_menu(|ui| {
                                            if ui.button("从快捷栏移除").clicked() {
                                                self.submit(
                                                    Action::SetShortcut {
                                                        id: profile.id,
                                                        enabled: false,
                                                    },
                                                    Pending::Update,
                                                );
                                                ui.close();
                                            }
                                        });
                                        if response.clicked() {
                                            self.open_profile(profile);
                                        }
                                    }
                                    if self.config.shortcut_profiles.is_empty() {
                                        ui.weak("点击 + 或拖入会话，添加到快捷栏");
                                    }
                                });
                            });
                        let max_offset =
                            (shortcuts.content_size.x - shortcuts.inner_rect.width()).max(0.0);
                        self.shortcut_offset =
                            shortcuts.state.clamped_offset().x.clamp(0.0, max_offset);
                        if ui
                            .add_enabled(
                                self.shortcut_offset < max_offset - 1.0,
                                egui::Button::new("▶").min_size(egui::vec2(22.0, 22.0)),
                            )
                            .on_hover_text("快捷栏向右翻页")
                            .clicked()
                        {
                            self.shortcut_offset =
                                (self.shortcut_offset + strip_width).min(max_offset);
                        }
                        let shortcut_target = ui.interact(
                            shortcuts.inner_rect,
                            ui.scope_id().with("shortcut_drop"),
                            egui::Sense::hover(),
                        );
                        if let Some(payload) = shortcut_target.dnd_hover_payload::<SessionDrag>()
                            && let SessionDrag::Profile(id) = *payload
                            && self.config.profiles.iter().any(|p| p.id == id)
                        {
                            ui.painter().rect_stroke(
                                shortcuts.inner_rect.shrink(1.0),
                                3.0,
                                egui::Stroke::new(2.0, ui.visuals().selection.bg_fill),
                                egui::StrokeKind::Inside,
                            );
                            if shortcut_target
                                .dnd_release_payload::<SessionDrag>()
                                .is_some()
                                && !self.config.shortcut_profiles.contains(&id)
                            {
                                self.submit(
                                    Action::SetShortcut { id, enabled: true },
                                    Pending::Update,
                                );
                            }
                        }
                    });
                }
                if let Some(action) = action {
                    self.chrome_action(action, &ctx);
                }
                if let Some(profile) = recent_profile {
                    self.open_profile(profile);
                }
                if let Some(export) = export_profile {
                    self.choose_session_file(&ctx, Some(export));
                }
            });
        }
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                if let Some(tab) = self.tabs.get(self.active) {
                    ui.colored_label(
                        if tab.connected {
                            Color32::from_rgb(0, 145, 40)
                        } else {
                            Color32::GRAY
                        },
                        &tab.status,
                    );
                    ui.separator();
                    ui.label(if tab.local {
                        "Windows · PowerShell · ConPTY".into()
                    } else {
                        tab.profile.endpoint()
                    });
                    ui.separator();
                    ui.label(format!("UTF-8  {} × {}", tab.size.0, tab.size.1));
                } else {
                    ui.label("就绪 · 配置保存在本机");
                }
            });
            ui.interact(
                ui.max_rect(),
                ui.scope_id().with("status_chrome_menu"),
                egui::Sense::click(),
            )
            .on_hover_text("右键可隐藏或恢复界面栏")
            .context_menu(|ui| self.draw_chrome_options(ui));
        });
        if self.show_sessions && !(self.show_ai_chat && self.ai_maximized) {
            egui::Panel::left("sessions")
                .frame(
                    egui::Frame::side_top_panel(ui.style()).fill(crate::ui_appearance::color(
                        crate::ui_appearance::colors(&self.config, !self.config.light_theme)
                            .sidebar,
                    )),
                )
                .default_size(
                    if self.smoke && std::env::args().any(|a| a == "--smoke-tunnel-status") {
                        180.0
                    } else {
                        280.0
                    },
                )
                .min_size((180.0 * self.sessions_zoom).max(180.0))
                .show(ui, |ui| {
                    if !self.settings
                        && self.editor.is_none()
                        && self.folder_editor.is_none()
                        && ui.rect_contains_pointer(ui.max_rect())
                    {
                        let steps = ui.input(|input| sidebar_zoom_steps(&input.events));
                        if steps != 0.0 {
                            let zoom = (self.sessions_zoom + steps * 0.1).clamp(0.75, 1.75);
                            if zoom != self.sessions_zoom
                                && self.submit(Action::SidebarZoom(zoom), Pending::Update)
                            {
                                self.sessions_zoom = zoom;
                            }
                            ui.input_mut(|input| input.smooth_scroll_delta = egui::Vec2::ZERO);
                        }
                    }
                    let zoom = self.sessions_zoom;
                    let mut style = (**ui.style()).clone();
                    for font in style.text_styles.values_mut() {
                        font.size *= zoom;
                    }
                    style.spacing.interact_size *= zoom;
                    style.spacing.button_padding *= zoom;
                    style.spacing.item_spacing *= zoom;
                    ui.set_style(style);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("会话管理器").size(12.0 * zoom));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add(egui::Button::new("«").frame(false))
                                .on_hover_text("隐藏会话管理器")
                                .clicked()
                            {
                                self.show_sessions = false;
                            }
                            if toolbar_icon(ui, 9, zoom).on_hover_text("设置").clicked() {
                                self.settings = true;
                            }
                            if toolbar_icon(ui, 10, zoom)
                                .on_hover_text("新建文件夹")
                                .clicked()
                            {
                                self.folder_editor = Some(Folder {
                                    id: Uuid::new_v4(),
                                    name: String::new(),
                                    parent_id: None,
                                });
                            }
                            if toolbar_icon(ui, 0, zoom)
                                .on_hover_text("新建会话")
                                .clicked()
                            {
                                self.open_profile_editor(self.new_profile(), false, None);
                            }
                        });
                    });
                    ui.separator();
                    ui.horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(12.0 * zoom, 20.0 * zoom),
                            egui::Sense::hover(),
                        );
                        let stroke = egui::Stroke::new(1.0, Color32::from_rgb(115, 149, 190));
                        let center = rect.center() - egui::vec2(zoom, 2.0 * zoom);
                        ui.painter().circle_stroke(center, 3.5 * zoom, stroke);
                        ui.painter().line_segment(
                            [
                                center + egui::vec2(2.5 * zoom, 2.5 * zoom),
                                center + egui::vec2(5.5 * zoom, 5.5 * zoom),
                            ],
                            stroke,
                        );
                        let has_filter = !self.filter.is_empty();
                        ui.add_sized(
                            [
                                ui.available_width() - if has_filter { 26.0 * zoom } else { 0.0 },
                                25.0 * zoom,
                            ],
                            egui::TextEdit::singleline(&mut self.filter).hint_text("搜索…"),
                        );
                        if has_filter && ui.small_button("×").on_hover_text("清除搜索").clicked()
                        {
                            self.filter.clear();
                        }
                    });
                    ui.add_space(4.0 * zoom);
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.spacing_mut().interact_size.y = 32.0 * zoom;
                    ui.spacing_mut().indent = 26.0 * zoom;
                    ui.visuals_mut().collapsing_header_frame = true;
                    ui.visuals_mut().widgets.inactive.bg_stroke = egui::Stroke::NONE;
                    ui.visuals_mut().widgets.hovered.bg_stroke = egui::Stroke::NONE;
                    ui.visuals_mut().widgets.active.bg_stroke = egui::Stroke::NONE;
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        let root = egui::CollapsingHeader::new("所有会话")
                            .id_salt(("all_sessions", !self.filter.is_empty()))
                            .default_open(true)
                            .open(if self.filter.is_empty() {
                                None
                            } else {
                                Some(true)
                            })
                            .show(ui, |ui| self.folder_tree(ui, None));
                        self.jumpserver_tree(ui);
                        if let Some(action) =
                            folder_drop(ui, &root.header_response, &self.config, None)
                        {
                            self.submit(action, Pending::Update);
                        }
                        if self.config.profiles.is_empty() {
                            ui.add_space(20.0);
                            ui.label(RichText::new("保存常用服务器").strong());
                            ui.label(
                                RichText::new(
                                    "新建会话可配置 SSH 或串口。\n也可在右侧终端直接输入：",
                                )
                                .weak(),
                            );
                            ui.code("ssh root@hostname");
                        }
                    });
                });
        }
        self.draw_ai_chat(ui);
        let dialog_blocks_terminal = !allow_terminal_input || self.ai_command.is_some();
        restore_terminal_focus_after_dialog(
            &mut self.terminal_focus_blocked,
            dialog_blocks_terminal,
            self.tabs.get_mut(self.active),
        );
        let mut local_reopen = false;
        let mut selection_for_ai = None;
        if !(self.show_ai_chat && self.ai_maximized) {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(ui.visuals().panel_fill))
                .show(ui, |ui| {
                    if self.terminal_layout != TerminalLayout::Single {
                        let previous = self.tabs.get(self.active).map(|tab| tab.id);
                        let response = terminal_panes(
                            ui,
                            &mut self.tabs,
                            &mut self.active,
                            &self.config,
                            self.terminal_layout,
                            &mut self.pane_groups,
                            allow_terminal_input && self.ai_command.is_none(),
                        );
                        selection_for_ai = response.selection;
                        local_reopen = response.new_local;
                        if let Some((index, action)) = response.action {
                            self.tab_action(index, action, &ctx);
                        }
                        if previous != self.tabs.get(self.active).map(|tab| tab.id) {
                            self.ai_command = None;
                        }
                        return;
                    }
                    if self.tabs.is_empty() {
                        ui.vertical_centered(|ui| {
                            ui.add_space(110.0);
                            ui.heading("打开一个终端");
                            ui.add_space(12.0);
                            ui.label("原生 Rust 桌面终端 · SSH 2 · 多标签会话");
                            ui.add_space(24.0);
                            if ui.button("+ 打开本地 PowerShell").clicked() {
                                local_reopen = true;
                            }
                        });
                    } else {
                        let mut tab_action = None;
                        let tab_count = self.tabs.len();
                        let has_local = self.tabs.iter().any(|tab| tab.local);
                        let has_disconnected = self
                            .tabs
                            .iter()
                            .any(|tab| !tab.local && tab.finished && !tab.connected);
                        let strip_width = (ui.available_width() - 50.0).max(60.0);
                        let tab_width =
                            ((strip_width - 24.0) / tab_count as f32).clamp(84.0, 140.0);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            ui.spacing_mut().interact_size.y = 20.0;
                            if ui
                                .add_enabled(
                                    tab_count > 1,
                                    egui::Button::new("◀").min_size(egui::vec2(20.0, 20.0)),
                                )
                                .on_hover_text("上一个标签 · Ctrl+PageUp / Ctrl+Alt+←")
                                .clicked()
                            {
                                self.switch_tab(-1, &ctx);
                            }
                            let active_id = self.tabs.get(self.active).map(|tab| tab.id);
                            let reveal_selected = self.visible_tab != active_id
                                || (self.tab_strip_width - strip_width).abs() > 1.0;
                            self.visible_tab = active_id;
                            self.tab_strip_width = strip_width;
                            let tabs = egui::ScrollArea::horizontal()
                                .id_salt("tabs")
                                .max_width(strip_width)
                                .auto_shrink([false, true])
                                .horizontal_scroll_offset(self.tab_scroll_offset)
                                .scroll_bar_visibility(
                                    egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                                )
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 0.0;
                                        for (index, tab) in self.tabs.iter_mut().enumerate() {
                                            let selected = index == self.active;
                                            let fill = if selected {
                                                ui.visuals().selection.bg_fill
                                            } else {
                                                ui.visuals().extreme_bg_color
                                            };
                                            let frame = egui::Frame::NONE
                                                .fill(fill)
                                                .stroke(egui::Stroke::new(
                                                    1.0,
                                                    ui.visuals()
                                                        .widgets
                                                        .noninteractive
                                                        .bg_stroke
                                                        .color,
                                                ))
                                                .inner_margin(egui::Margin::symmetric(3, 1))
                                                .show(ui, |ui| {
                                                    ui.horizontal(|ui| {
                                                        ui.spacing_mut().item_spacing.x = 1.0;
                                                        ui.spacing_mut().button_padding =
                                                            egui::vec2(1.0, 0.0);
                                                        ui.colored_label(
                                                            if tab.connected {
                                                                Color32::from_rgb(0, 190, 40)
                                                            } else if tab.finished {
                                                                Color32::from_rgb(245, 65, 70)
                                                            } else {
                                                                Color32::GRAY
                                                            },
                                                            RichText::new("●").size(11.0),
                                                        );
                                                        let text = RichText::new(format!(
                                                            "{} {}",
                                                            index + 1,
                                                            tab.title
                                                        ))
                                                        .size(12.0)
                                                        .color(if selected {
                                                            ui.visuals().selection.stroke.color
                                                        } else {
                                                            ui.visuals().text_color()
                                                        });
                                                        let response = ui
                                                            .add_sized(
                                                                [tab_width - 36.0, 18.0],
                                                                egui::Button::new(text)
                                                                    .frame(false)
                                                                    .truncate(),
                                                            )
                                                            .on_hover_text(&tab.title);
                                                        response.context_menu(|ui| {
                                                            for (label, action, enabled) in [
                                                                (
                                                                    "新选项卡(N)    Shift+Alt+N",
                                                                    TabAction::New,
                                                                    true,
                                                                ),
                                                                (
                                                                    "复制会话(D)",
                                                                    TabAction::Duplicate,
                                                                    true,
                                                                ),
                                                                (
                                                                    "复制 SSH 渠道(H)",
                                                                    TabAction::Channel,
                                                                    !tab.local
                                                                        && tab.profile.protocol
                                                                            == Protocol::Ssh
                                                                        && tab.connected
                                                                        && !tab.profile.tunnel_only,
                                                                ),
                                                                (
                                                                    "清屏",
                                                                    TabAction::ClearScreen,
                                                                    true,
                                                                ),
                                                                (
                                                                    "关闭(C)    Ctrl+Shift+F4",
                                                                    TabAction::Close(
                                                                        CloseTabs::Current,
                                                                    ),
                                                                    true,
                                                                ),
                                                                (
                                                                    "关闭右侧标签页(R)",
                                                                    TabAction::Close(
                                                                        CloseTabs::Right,
                                                                    ),
                                                                    index + 1 < tab_count,
                                                                ),
                                                                (
                                                                    "关闭所有本地标签(L)",
                                                                    TabAction::Close(
                                                                        CloseTabs::Local,
                                                                    ),
                                                                    has_local,
                                                                ),
                                                                (
                                                                    "关闭断开的连接",
                                                                    TabAction::Close(
                                                                        CloseTabs::Disconnected,
                                                                    ),
                                                                    has_disconnected,
                                                                ),
                                                                (
                                                                    "关闭其他选项卡(O)",
                                                                    TabAction::Close(
                                                                        CloseTabs::Others,
                                                                    ),
                                                                    tab_count > 1,
                                                                ),
                                                                (
                                                                    "关闭所有选项卡(B)",
                                                                    TabAction::Close(
                                                                        CloseTabs::All,
                                                                    ),
                                                                    true,
                                                                ),
                                                                (
                                                                    "重命名(M)…",
                                                                    TabAction::Rename,
                                                                    true,
                                                                ),
                                                            ] {
                                                                if ui
                                                                    .add_enabled(
                                                                        enabled,
                                                                        egui::Button::new(label),
                                                                    )
                                                                    .clicked()
                                                                {
                                                                    tab_action =
                                                                        Some((index, action));
                                                                    ui.close();
                                                                }
                                                            }
                                                        });
                                                        if response.clicked() {
                                                            self.active = index;
                                                            tab.focus = true;
                                                            response.scroll_to_me(None);
                                                        }
                                                        if ui
                                                            .add_sized(
                                                                [18.0, 18.0],
                                                                egui::Button::new(
                                                                    RichText::new("×")
                                                                        .size(12.0)
                                                                        .color(if selected {
                                                                            ui.visuals()
                                                                                .selection
                                                                                .stroke
                                                                                .color
                                                                        } else {
                                                                            ui.visuals()
                                                                                .text_color()
                                                                        }),
                                                                )
                                                                .frame(false),
                                                            )
                                                            .on_hover_text("关闭标签并断开连接")
                                                            .clicked()
                                                        {
                                                            tab_action = Some((
                                                                index,
                                                                TabAction::Close(
                                                                    CloseTabs::Current,
                                                                ),
                                                            ));
                                                        }
                                                    });
                                                });
                                            if selected && reveal_selected {
                                                frame.response.scroll_to_me(None);
                                            }
                                        }
                                        if ui
                                            .add_sized(
                                                [20.0, 20.0],
                                                egui::Button::new("+").frame(false),
                                            )
                                            .on_hover_text("新建本地终端")
                                            .clicked()
                                        {
                                            local_reopen = true;
                                        }
                                    });
                                });
                            let max_offset =
                                (tabs.content_size.x - tabs.inner_rect.width()).max(0.0);
                            self.tab_scroll_offset =
                                tabs.state.clamped_offset().x.clamp(0.0, max_offset);
                            let wheel = ui.input(|input| {
                                if !input.modifiers.ctrl
                                    && input
                                        .pointer
                                        .hover_pos()
                                        .is_some_and(|pos| tabs.inner_rect.contains(pos))
                                {
                                    input.smooth_scroll_delta.y
                                } else {
                                    0.0
                                }
                            });
                            self.tab_scroll_offset =
                                (self.tab_scroll_offset - wheel).clamp(0.0, max_offset);
                            if ui
                                .add_enabled(
                                    tab_count > 1,
                                    egui::Button::new("▶").min_size(egui::vec2(20.0, 20.0)),
                                )
                                .on_hover_text("下一个标签 · Ctrl+PageDown / Ctrl+Alt+→")
                                .clicked()
                            {
                                self.switch_tab(1, &ctx);
                            }
                        });
                        if let Some((index, action)) = tab_action {
                            self.tab_action(index, action, &ctx);
                        }
                        if let Some(tab) = self.tabs.get_mut(self.active) {
                            if !tab.connected {
                                ui.horizontal_wrapped(|ui| {
                                    ui.colored_label(
                                        if tab.finished {
                                            Color32::LIGHT_RED
                                        } else {
                                            Color32::GRAY
                                        },
                                        &tab.status,
                                    );
                                });
                            }
                            let selected = crate::terminal_view::show(
                                ui,
                                tab,
                                &self.config,
                                allow_terminal_input
                                    && self.ai_command.is_none()
                                    && !self.keys.open
                                    && !self.keys.hosts_open
                                    && !self.settings
                                    && self.rename.is_none()
                                    && self.editor.is_none()
                                    && self.folder_editor.is_none(),
                            );
                            if let Some(text) = selected {
                                selection_for_ai = Some((tab.id, text));
                            }
                        }
                    }
                });
        }
        if let Some((tab_id, text)) = selection_for_ai {
            self.add_terminal_selection_to_ai(tab_id, &text);
        }
        if local_reopen {
            self.start_local(&ctx, None);
        }
        self.rename_dialog(&ctx);
        self.folder_dialog(&ctx);
        self.editor(&ctx);
        self.trust_new_hosts();
        if let Some(id) = self.delete {
            egui::Modal::new(egui::Id::unique("delete")).show(&ctx, |ui| {
                ui.heading("删除保存的会话？");
                ui.label("正在运行的连接将保留，主机指纹不会删除。");
                ui.horizontal(|ui| {
                    if ui.button("取消").clicked() {
                        self.delete = None;
                    }
                    if ui.button("删除").clicked()
                        && self.submit(Action::DeleteProfile(id), Pending::Delete)
                    {
                        self.delete = None;
                    }
                });
            });
        }
        self.draw_settings(&ctx);
        self.draw_key_managers(&ctx);
        self.session_confirmations(&ctx);
        self.draw_ai_editor(&ctx);
        self.draw_ai_command_confirmation(&ctx);
        self.draw_updates(&ctx);
        if self.about_open {
            egui::Modal::new(egui::Id::unique("about_rshell")).show(&ctx, |ui| {
                ui.set_max_width(360.0);
                ui.heading("关于 RShell");
                ui.label("RShell · 原生 Rust 终端");
                ui.label(concat!("软件版本：", env!("CARGO_PKG_VERSION")));
                ui.separator();
                ui.label("地址栏输入主机或已保存会话名称，Enter 打开连接设置。");
                if ui.button("关闭").clicked() {
                    self.about_open = false;
                }
            });
        }
        self.host_editor(&ctx);
        if !self.message.is_empty() {
            egui::Window::new("提示")
                .collapsible(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(&ctx, |ui| {
                    ui.label(&self.message);
                    if ui.button("确定").clicked() {
                        self.message.clear();
                    }
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjacent_tab_wraps_in_both_directions() {
        assert_eq!(adjacent_tab(0, 4, -1), 3);
        assert_eq!(adjacent_tab(3, 4, 1), 0);
        assert_eq!(adjacent_tab(1, 4, 1), 2);
        assert_eq!(adjacent_tab(0, 0, 1), 0);
    }

    #[test]
    fn drag_targets_reject_cycles_and_no_op_moves() {
        let parent = Folder {
            id: Uuid::new_v4(),
            name: "生产".into(),
            parent_id: None,
        };
        let child = Folder {
            id: Uuid::new_v4(),
            name: "数据库".into(),
            parent_id: Some(parent.id),
        };
        let profile = Profile {
            folder_id: Some(child.id),
            ..Default::default()
        };
        let config = Config {
            folders: vec![parent.clone(), child.clone()],
            profiles: vec![profile.clone()],
            ..Default::default()
        };
        assert!(move_action(&config, SessionDrag::Folder(parent.id), Some(child.id)).is_none());
        assert!(move_action(&config, SessionDrag::Folder(child.id), Some(child.id)).is_none());
        assert!(move_action(&config, SessionDrag::Profile(profile.id), Some(child.id)).is_none());
        assert!(
            matches!(move_action(&config, SessionDrag::Folder(child.id), None), Some(Action::SaveFolder(folder)) if folder.parent_id.is_none())
        );
        assert!(matches!(
            move_action(&config, SessionDrag::Profile(profile.id), None),
            Some(Action::MoveProfile {
                folder_id: None,
                ..
            })
        ));
    }

    #[test]
    fn remembered_login_and_edit_have_distinct_behavior() {
        let mut editor = Editor::new(Profile {
            host: "127.0.0.1".into(),
            username: "test".into(),
            remember_secret: true,
            ..Default::default()
        });
        assert_eq!(editor.enter_action(), 2);
        assert!(!editor.accept_secret(Some(Zeroizing::new("test-secret".into()))));
        editor.connect_after_load = true;
        assert!(editor.accept_secret(Some(Zeroizing::new("test-secret".into()))));
        assert!(!editor.accept_secret(None));
        editor.profile.host.clear();
        assert!(!editor.accept_secret(Some(Zeroizing::new("test-secret".into()))));
        editor.profile.remember_secret = false;
        assert_eq!(editor.enter_action(), 3);
    }

    fn frame(ctx: &egui::Context, tab: &mut Tab, events: Vec<egui::Event>, allow: bool) {
        frame_config(ctx, tab, events, allow, &Config::default());
    }

    fn frame_config(
        ctx: &egui::Context,
        tab: &mut Tab,
        events: Vec<egui::Event>,
        allow: bool,
        config: &Config,
    ) -> egui::FullOutput {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            events,
            focused: true,
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |root| {
            egui::CentralPanel::default().show(root, |ui| {
                crate::terminal_view::show(ui, tab, config, allow);
            });
        });
        // Headless input tests intentionally have no GPU texture consumer.
        output.textures_delta.clear();
        output
    }

    #[test]
    fn terminal_regains_keyboard_focus_after_dialog_closes() {
        let (ctx, mut tab) = setup();
        tab.focus = false;
        let mut was_blocked = false;
        restore_terminal_focus_after_dialog(&mut was_blocked, true, Some(&mut tab));
        frame(&ctx, &mut tab, vec![], false);
        restore_terminal_focus_after_dialog(&mut was_blocked, false, Some(&mut tab));
        frame(&ctx, &mut tab, vec![], true);
        frame(&ctx, &mut tab, vec![egui::Event::Text("a".into())], true);
        assert_eq!(written(&tab), b"a");
    }

    #[test]
    fn left_click_clears_terminal_selection_but_right_click_keeps_it() {
        let (ctx, mut tab) = setup();
        {
            let mut terminal = tab.terminal.lock().unwrap();
            terminal.feed(b"selected text");
            terminal.start_selection(0, 0, rshell_terminal::Side::Left);
            terminal.extend_selection(0, 7, rshell_terminal::Side::Right);
            assert!(!terminal.selection_text().is_empty());
        }
        for button in [egui::PointerButton::Secondary, egui::PointerButton::Primary] {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                events: vec![egui::Event::PointerButton {
                    pos: egui::pos2(20.0, 20.0),
                    button,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                }],
                focused: true,
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |root| {
                clear_selection_on_primary_click(root.ctx(), Some(&mut tab));
            });
            output.textures_delta.clear();
            assert_eq!(
                tab.terminal.lock().unwrap().selection_text().is_empty(),
                button == egui::PointerButton::Primary
            );
        }
    }

    fn zoom_events(ctrl: bool, delta: f32) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(egui::pos2(400.0, 300.0)),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: egui::vec2(0.0, delta),
                modifiers: if ctrl {
                    egui::Modifiers::CTRL
                } else {
                    egui::Modifiers::NONE
                },
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Unknown,
            },
        ]
    }

    #[test]
    fn sidebar_zoom_requires_ctrl() {
        assert_eq!(sidebar_zoom_steps(&zoom_events(true, 2.0)), 2.0);
        assert_eq!(sidebar_zoom_steps(&zoom_events(false, 2.0)), 0.0);
    }

    #[test]
    fn ctrl_wheel_scales_only_current_ssh_terminal_and_resizes_pty() {
        let (ctx, mut tab) = setup();
        let original_size = tab.size;
        frame(&ctx, &mut tab, zoom_events(true, 2.0), true);
        assert_eq!(tab.font_size_override, Some(18.0));
        assert_eq!(ctx.zoom_factor(), 1.0);
        assert!(tab.size.0 < original_size.0 && tab.size.1 < original_size.1);
        assert!(
            tab.test_sent
                .iter()
                .any(|c| matches!(c, ssh::Command::Resize { .. }))
        );
        assert!(
            !tab.test_sent
                .iter()
                .any(|c| matches!(c, ssh::Command::Input(_)))
        );
        let other = App::make_tab(Profile::default());
        assert_eq!(other.font_size_override, None);
        frame(&ctx, &mut tab, zoom_events(true, 100.0), true);
        assert_eq!(tab.font_size_override, Some(32.0));
        frame(&ctx, &mut tab, zoom_events(true, -100.0), true);
        assert_eq!(tab.font_size_override, Some(10.0));
    }

    #[test]
    fn ssh_cursor_scales_and_stays_aligned_with_terminal_grid() {
        fn cursor_rect(output: &egui::FullOutput) -> egui::Rect {
            fn find(shape: &egui::Shape) -> Option<egui::Rect> {
                match shape {
                    egui::Shape::Rect(rect) if rect.fill == egui::Color32::from_rgb(0, 255, 0) => {
                        Some(rect.rect)
                    }
                    egui::Shape::Vec(shapes) => shapes.iter().find_map(find),
                    _ => None,
                }
            }
            output
                .shapes
                .iter()
                .find_map(|clipped| find(&clipped.shape))
                .unwrap()
        }

        let (ctx, mut tab) = setup();
        tab.terminal.lock().unwrap().feed(b"abc");
        let config = Config::default();
        let normal = cursor_rect(&frame_config(&ctx, &mut tab, vec![], true, &config));
        let enlarged = cursor_rect(&frame_config(
            &ctx,
            &mut tab,
            zoom_events(true, 8.0),
            true,
            &config,
        ));
        assert!(enlarged.width() > normal.width() && enlarged.height() > normal.height());
        let reduced = cursor_rect(&frame_config(
            &ctx,
            &mut tab,
            zoom_events(true, -14.0),
            true,
            &config,
        ));
        assert!(reduced.width() < normal.width() && reduced.height() < normal.height());
        for rect in [normal, enlarged, reduced] {
            // CentralPanel margin (8) + terminal padding (8); cursor follows column 3.
            assert!((rect.left() - (16.0 + 3.0 * rect.width())).abs() < 0.01);
            assert!((rect.top() - 16.0).abs() < 0.01);
        }
    }

    #[test]
    fn ctrl_wheel_scales_local_powershell_and_resizes_conpty() {
        let (ctx, mut tab) = setup();
        tab.local = true;
        let original_size = tab.size;
        frame(&ctx, &mut tab, zoom_events(true, 2.0), true);
        assert_eq!(tab.font_size_override, Some(18.0));
        assert!(tab.size.0 < original_size.0 && tab.size.1 < original_size.1);
        assert!(
            tab.test_sent
                .iter()
                .any(|command| matches!(command, ssh::Command::Resize { .. }))
        );
        assert!(
            !tab.test_sent
                .iter()
                .any(|command| matches!(command, ssh::Command::Input(_)))
        );
        assert_eq!(ctx.zoom_factor(), 1.0);
    }

    #[test]
    fn wheel_zoom_ignores_normal_scroll_dialogs_and_serial_tabs() {
        let (ctx, mut tab) = setup();
        frame(&ctx, &mut tab, zoom_events(false, 1.0), true);
        frame(&ctx, &mut tab, zoom_events(true, 1.0), false);
        assert_eq!(tab.font_size_override, None);
        tab.profile.protocol = Protocol::Serial;
        frame(&ctx, &mut tab, zoom_events(true, 1.0), true);
        assert_eq!(tab.font_size_override, None);
    }

    #[test]
    fn hidden_bars_restore_from_status_and_history_applies_to_existing_and_new_tabs() {
        use eframe::App as _;
        let root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/settings-gui-tests");
        std::fs::create_dir_all(&root).unwrap();
        let temp = tempfile::tempdir_in(root).unwrap();
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let config = Config {
            scrollback_lines: 3,
            chrome: rshell_core::config::ChromeVisibility {
                menu: false,
                toolbar: false,
                address: false,
                shortcuts: false,
            },
            ..Default::default()
        };
        let mut app = App::new(
            &cc,
            config,
            temp.path().join("config.json"),
            runtime,
            true,
            false,
        )
        .unwrap();
        let render = |app: &mut App, events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000.0, 700.0),
                    )),
                    events,
                    focused: true,
                    ..Default::default()
                },
                |ui| app.ui(ui, &mut eframe::Frame::_new_kittest()),
            );
            output.textures_delta.clear();
            output
        };
        let output = render(&mut app, vec![]);
        assert!(!output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text == "文件(F)")));
        let pos = egui::pos2(50.0, 690.0);
        for pressed in [true, false] {
            render(
                &mut app,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        pressed,
                        button: egui::PointerButton::Secondary,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        let output = render(&mut app, vec![]);
        let restore = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == "显示全部栏" => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                _ => None,
            })
            .expect("status bar must provide a recovery menu when all top bars are hidden");
        for pressed in [true, false] {
            render(
                &mut app,
                vec![
                    egui::Event::PointerMoved(restore),
                    egui::Event::PointerButton {
                        pos: restore,
                        pressed,
                        button: egui::PointerButton::Primary,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        for _ in 0..200 {
            app.poll_store(&ctx);
            if app.pending.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            app.config.chrome,
            rshell_core::config::ChromeVisibility::default()
        );
        let output = render(&mut app, vec![]);
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text == "文件(F)")));
        for _ in 0..50 {
            app.tabs[0].terminal.lock().unwrap().feed(b"line\r\n");
        }
        assert_eq!(
            app.tabs[0].terminal.lock().unwrap().snapshot().history_size,
            3
        );
        assert!(app.submit(Action::ScrollbackLines(12), Pending::Update));
        for _ in 0..200 {
            app.poll_store(&ctx);
            if app.pending.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(app.config.scrollback_lines, 12);
        let new = app.configured_tab(Profile::default());
        for terminal in [&app.tabs[0].terminal, &new.terminal] {
            for _ in 0..50 {
                terminal.lock().unwrap().feed(b"line\r\n");
            }
            assert_eq!(terminal.lock().unwrap().snapshot().history_size, 12);
        }
        assert_eq!(Config::load(&app.path).unwrap().scrollback_lines, 12);
    }

    #[test]
    fn menu_restore_shortcut_is_consumed_before_terminal_input() {
        let ctx = egui::Context::default();
        let modifiers = egui::Modifiers::CTRL | egui::Modifiers::SHIFT;
        let mut consumed = false;
        let mut output = ctx.run_ui(
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key: egui::Key::M,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                }],
                ..Default::default()
            },
            |_| {
                consumed = consume_menu_visibility_shortcut(&ctx);
                assert!(!ctx.input(|input| input.key_pressed(egui::Key::M)));
            },
        );
        output.textures_delta.clear();
        assert!(consumed);
    }

    #[test]
    fn split_dividers_drag_resize_and_block_terminal_input() {
        for (layout, start, end) in [
            (
                TerminalLayout::Columns,
                egui::pos2(400.0, 150.0),
                egui::pos2(510.0, 150.0),
            ),
            (
                TerminalLayout::Rows,
                egui::pos2(150.0, 300.0),
                egui::pos2(150.0, 400.0),
            ),
            (
                TerminalLayout::Grid,
                egui::pos2(400.0, 150.0),
                egui::pos2(510.0, 150.0),
            ),
            (
                TerminalLayout::Grid,
                egui::pos2(150.0, 300.0),
                egui::pos2(150.0, 400.0),
            ),
        ] {
            let ctx = egui::Context::default();
            let count = if layout == TerminalLayout::Grid { 4 } else { 2 };
            let mut tabs: Vec<_> = (0..count)
                .map(|_| App::make_tab(Profile::default()))
                .collect();
            for tab in &mut tabs {
                tab.connected = true;
            }
            let mut active = 0;
            let mut groups = PaneGroups::default();
            let mut render = |events, allow, tabs: &mut Vec<Tab>| {
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
                    |root| {
                        egui::CentralPanel::default().show(root, |ui| {
                            terminal_panes(
                                ui,
                                tabs,
                                &mut active,
                                &Config::default(),
                                layout,
                                &mut groups,
                                allow,
                            );
                        });
                    },
                );
                output.textures_delta.clear();
            };
            render(vec![], true, &mut tabs);
            render(vec![egui::Event::PointerMoved(start)], true, &mut tabs);
            let before = tabs[0].size;
            let button = |pos, pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            render(vec![button(start, true)], true, &mut tabs);
            render(
                vec![
                    egui::Event::PointerMoved(end),
                    egui::Event::Text("blocked".into()),
                ],
                true,
                &mut tabs,
            );
            let resized = tabs[0].size;
            if start.x != end.x {
                assert!(resized.0 > before.0);
            } else {
                assert!(resized.1 > before.1);
            }
            assert!(tabs.iter().all(|tab| written(tab).is_empty()));
            render(vec![button(end, false)], true, &mut tabs);
            render(vec![], true, &mut tabs);
            assert_eq!(tabs[0].size, resized);
            // A dialog disables both divider dragging and terminal input.
            render(
                vec![egui::Event::PointerMoved(end), button(end, true)],
                false,
                &mut tabs,
            );
            render(vec![egui::Event::PointerMoved(start)], false, &mut tabs);
            assert_eq!(tabs[0].size, resized);
            render(vec![button(start, false)], false, &mut tabs);
        }
    }

    #[test]
    fn split_panes_route_input_only_to_clicked_session_and_resize() {
        let ctx = egui::Context::default();
        let mut tabs = vec![
            App::make_tab(Profile::default()),
            App::make_tab(Profile::default()),
        ];
        for tab in &mut tabs {
            tab.connected = true;
        }
        let mut active = 0;
        let mut groups = PaneGroups::default();
        let mut render = |events, layout, allow, tabs: &mut Vec<Tab>| {
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
                |root| {
                    egui::CentralPanel::default().show(root, |ui| {
                        terminal_panes(
                            ui,
                            tabs,
                            &mut active,
                            &Config::default(),
                            layout,
                            &mut groups,
                            allow,
                        );
                    });
                },
            );
            output.textures_delta.clear();
        };
        render(vec![], TerminalLayout::Columns, true, &mut tabs);
        render(
            vec![egui::Event::Text("a".into())],
            TerminalLayout::Columns,
            true,
            &mut tabs,
        );
        assert_eq!(written(&tabs[0]), b"a");
        assert!(written(&tabs[1]).is_empty());
        let before = tabs[0].size;
        let pos = egui::pos2(600.0, 150.0);
        render(
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            TerminalLayout::Columns,
            true,
            &mut tabs,
        );
        render(
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            TerminalLayout::Columns,
            true,
            &mut tabs,
        );
        render(
            vec![egui::Event::Text("b".into())],
            TerminalLayout::Columns,
            true,
            &mut tabs,
        );
        assert_eq!(written(&tabs[0]), b"a");
        assert_eq!(written(&tabs[1]), b"b");
        render(
            vec![egui::Event::Text("blocked".into())],
            TerminalLayout::Rows,
            false,
            &mut tabs,
        );
        assert_eq!(written(&tabs[1]), b"b");
        assert!(tabs[0].size.0 > before.0);
        assert!(tabs[0].size.1 < before.1);
    }

    #[test]
    fn starting_terminal_does_not_wait_on_resize_lock_or_queue_input() {
        let (ctx, mut tab) = setup();
        tab.local = true;
        tab.connected = false;
        tab.size = (0, 0);
        let terminal = tab.terminal.clone();
        let guard = terminal.lock().unwrap();
        frame(
            &ctx,
            &mut tab,
            vec![egui::Event::Text("Get-Process".into())],
            true,
        );
        assert_eq!(tab.size, (0, 0));
        assert!(tab.test_sent.is_empty());
        drop(guard);
        frame(&ctx, &mut tab, vec![], true);
        assert_ne!(tab.size, (0, 0));
    }

    #[test]
    fn remote_exit_keeps_shell_target_for_toolbar_reconnect() {
        use eframe::App as _;
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target");
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
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let profile = Profile {
            name: "loopback-shell".into(),
            protocol: Protocol::Telnet,
            host: "127.0.0.1".into(),
            port: listener.local_addr().unwrap().port(),
            ..Profile::default()
        };
        app.start_connection(&ctx, profile.clone(), String::new(), None);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !app.tabs[app.active].connected {
            app.poll();
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!app.config.session_options.clear_scrollback_on_close);
        let terminal = app.tabs[app.active].terminal.clone();
        {
            let mut terminal = terminal.lock().unwrap();
            terminal.feed(b"retained-history\r\n");
            terminal.feed("output\r\n".repeat(80).as_bytes());
            terminal.feed(b"retained-screen");
        }
        let history_size = terminal.lock().unwrap().snapshot().history_size;
        assert!(history_size > 0);
        drop(listener.accept().unwrap());
        while !app.tabs[app.active].finished {
            app.poll();
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let tab = &app.tabs[app.active];
        assert!(!tab.local && !tab.connected);
        assert_eq!(tab.title, profile.name);
        {
            let mut terminal = terminal.lock().unwrap();
            assert_eq!(terminal.snapshot().history_size, history_size);
            assert!(terminal.find("retained-history", None, false).is_some());
            assert!(terminal.find("retained-screen", None, false).is_some());
            terminal.bottom();
        }
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            app.ui(ui, &mut eframe::Frame::_new_kittest())
        });
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text.contains("已断开"))));
        output.textures_delta.clear();
        for visuals in [egui::Visuals::dark(), egui::Visuals::light()] {
            ctx.set_visuals(visuals);
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800.0, 600.0),
                    )),
                    ..Default::default()
                },
                |ui| app.ui(ui, &mut eframe::Frame::_new_kittest()),
            );
            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
                egui::Shape::Text(text) if text.galley.job.text.contains("已断开"))));
            assert!(
                terminal
                    .lock()
                    .unwrap()
                    .find("retained-screen", None, false)
                    .is_some()
            );
            output.textures_delta.clear();
        }
        let original = app.tabs[app.active].id;
        let count = app.tabs.len();
        app.chrome_action(2, &ctx);
        let target = &app.editor.as_ref().unwrap().profile;
        assert_eq!(target.id, profile.id);
        assert_eq!((&target.host, target.port), (&profile.host, profile.port));
        let editor = app.editor.take().unwrap();
        assert_eq!(editor.reconnect_tab, Some(original));
        app.start_connection(
            &ctx,
            editor.profile.clone(),
            String::new(),
            editor.reconnect_tab,
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !app.tabs[app.active].connected {
            app.poll();
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(app.tabs.len(), count);
        assert_eq!(app.tabs[app.active].id, original);
        // Explicit opt-in still clears the terminal when the connection closes.
        app.config.session_options.clear_scrollback_on_close = true;
        app.tabs[app.active]
            .terminal
            .lock()
            .unwrap()
            .feed(b"clear-on-request");
        app.tabs[app.active]
            .connection
            .as_ref()
            .unwrap()
            .disconnect();
        while !app.tabs[app.active].finished {
            app.poll();
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            app.tabs[app.active]
                .terminal
                .lock()
                .unwrap()
                .find("clear-on-request", None, false)
                .is_none()
        );
        let index = app.active;
        close_tabs(&mut app.tabs, &mut app.active, index, CloseTabs::Current);
        assert!(
            app.start_connection(&ctx, profile, String::new(), Some(original))
                .is_none()
        );
        assert_eq!(app.tabs.len(), count - 1);
    }

    #[test]
    fn local_tab_and_menu_remain_available_before_worker_starts() {
        use eframe::App as _;
        let root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/local-startup-tests");
        std::fs::create_dir_all(&root).unwrap();
        let temp = tempfile::tempdir_in(root).unwrap();
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(1)
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
        let (release, blocked) = std::sync::mpsc::channel::<()>();
        let (entered, started) = std::sync::mpsc::channel();
        app.runtime.spawn_blocking(move || {
            let _ = entered.send(());
            let _ = blocked.recv();
        });
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        let old_count = app.tabs.len();
        app.start_local(&ctx, None);
        assert_eq!(app.tabs.len(), old_count + 1);
        assert_eq!(app.active, old_count);
        assert!(!app.tabs[old_count].connected);
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 700.0),
                )),
                ..Default::default()
            },
            |ui| app.ui(ui, &mut eframe::Frame::_new_kittest()),
        );
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text.contains("PowerShell"))));
        output.textures_delta.clear();
        let sessions = app.show_sessions;
        app.chrome_action(7, &ctx);
        assert_ne!(app.show_sessions, sessions);
        app.switch_tab(-1, &ctx);
        assert_ne!(app.active, old_count);
        app.tabs[old_count]
            .connection
            .as_ref()
            .unwrap()
            .disconnect();
        drop(release);
    }

    #[test]
    fn terminal_frame_reuses_snapshot_while_parser_holds_lock() {
        let (ctx, mut tab) = setup();
        let terminal = tab.terminal.clone();
        let guard = terminal.lock().unwrap();
        assert!(tab.render_snapshot.is_some());
        frame(&ctx, &mut tab, vec![], false);
        drop(guard);
    }

    #[test]
    fn tunnel_only_tabs_never_accept_terminal_or_ai_commands() {
        let (ctx, mut tab) = setup();
        tab.profile.tunnel_only = true;
        tab.profile
            .tunnels
            .push(rshell_core::tunnel::Rule::default());
        tab.test_sent.clear();
        assert!(!tab.command_ready());
        tab.input(b"dangerous-command\r".to_vec());
        frame(
            &ctx,
            &mut tab,
            vec![egui::Event::Text("whoami".into())],
            true,
        );
        assert!(tab.test_sent.is_empty());
        tab.profile.tunnel_only = false;
        assert!(tab.command_ready());
    }

    #[test]
    #[ignore = "manual rendering benchmark"]
    fn terminal_render_benchmark() {
        let (ctx, mut tab) = setup();
        tab.terminal.lock().unwrap().resize(tab.size.0, tab.size.1);
        let line = "abcdefghijklmnopqrstuvwxyz 0123456789 ABCDEFGHIJKLMNOPQRSTUVWXYZ\r\n";
        tab.terminal
            .lock()
            .unwrap()
            .feed(line.repeat(100).as_bytes());
        for _ in 0..10 {
            frame(&ctx, &mut tab, vec![], false);
        }
        let start = std::time::Instant::now();
        let mut shapes = 0;
        for _ in 0..200 {
            shapes = frame_config(&ctx, &mut tab, vec![], false, &Config::default())
                .shapes
                .len();
        }
        eprintln!(
            "terminal_render: {:.3} ms/frame, {shapes} shapes",
            start.elapsed().as_secs_f64() * 1000.0 / 200.0
        );
    }

    fn setup() -> (egui::Context, Tab) {
        let ctx = egui::Context::default();
        let mut tab = App::make_tab(Profile::default());
        tab.connected = true;
        frame(&ctx, &mut tab, vec![], true);
        tab.test_sent.clear();
        (ctx, tab)
    }

    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }
    fn written(tab: &Tab) -> Vec<u8> {
        tab.test_sent
            .iter()
            .filter_map(|cmd| {
                if let ssh::Command::Input(bytes) = cmd {
                    Some(bytes.as_slice())
                } else {
                    None
                }
            })
            .flatten()
            .copied()
            .collect()
    }

    #[test]
    fn gui_arrows_tab_escape_and_text_are_sent_once() {
        let (ctx, mut tab) = setup();
        frame(
            &ctx,
            &mut tab,
            vec![
                key(egui::Key::A),
                egui::Event::Text("a".into()),
                key(egui::Key::ArrowUp),
                key(egui::Key::Tab),
                key(egui::Key::Escape),
            ],
            true,
        );
        assert_eq!(written(&tab), b"a\x1b[A\t\x1b");
    }

    #[test]
    fn ime_preedit_is_local_and_commit_is_sent_once() {
        let (ctx, mut tab) = setup();
        frame(
            &ctx,
            &mut tab,
            vec![egui::Event::Ime(egui::ImeEvent::Preedit {
                text: "nihao".into(),
                active_range_chars: None,
            })],
            true,
        );
        assert!(written(&tab).is_empty());
        frame(
            &ctx,
            &mut tab,
            vec![egui::Event::Ime(egui::ImeEvent::Commit("你好".into()))],
            true,
        );
        assert_eq!(written(&tab), "你好".as_bytes());
    }

    #[test]
    fn modal_blocks_terminal_input_and_multiline_paste_needs_confirmation() {
        let (ctx, mut tab) = setup();
        frame(
            &ctx,
            &mut tab,
            vec![egui::Event::Text("secret-in-dialog".into())],
            false,
        );
        assert!(written(&tab).is_empty());
        frame(
            &ctx,
            &mut tab,
            vec![egui::Event::Paste("echo a\necho b".into())],
            true,
        );
        assert!(written(&tab).is_empty());
        assert!(tab.pending_paste.is_some());
    }

    #[test]
    fn ctrl_v_reaches_terminal_and_explicit_paste_stays_available() {
        let (ctx, mut tab) = setup();
        for alternate in [false, true] {
            if alternate {
                tab.terminal.lock().unwrap().feed(b"\x1b[?1049h");
            }
            tab.test_sent.clear();
            let mut input = egui::RawInput {
                events: vec![egui::Event::Key {
                    key: egui::Key::V,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::CTRL,
                }],
                ..Default::default()
            };
            text_edit_clipboard_shortcuts(&ctx, &mut input);
            frame(&ctx, &mut tab, input.events, true);
            assert_eq!(written(&tab), [22]);
            assert!(tab.pending_paste.is_none());
        }
        for (key, modifiers) in [
            (egui::Key::V, egui::Modifiers::CTRL | egui::Modifiers::SHIFT),
            (egui::Key::Insert, egui::Modifiers::SHIFT),
        ] {
            tab.test_sent.clear();
            let output = frame_config(
                &ctx,
                &mut tab,
                vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                }],
                true,
                &Config::default(),
            );
            assert!(written(&tab).is_empty());
            assert!(output.viewport_output.values().any(|viewport| {
                viewport
                    .commands
                    .iter()
                    .any(|cmd| matches!(cmd, egui::ViewportCommand::RequestPaste))
            }));
        }
    }

    #[test]
    fn text_fields_keep_native_clipboard_shortcuts() {
        let ctx = egui::Context::default();
        let mut text = String::from("editable");
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.add(egui::TextEdit::singleline(&mut text))
                .request_focus();
        });
        output.textures_delta.clear();
        let mut input = egui::RawInput {
            events: [egui::Key::C, egui::Key::X, egui::Key::V]
                .into_iter()
                .map(|key| egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::CTRL,
                })
                .collect(),
            ..Default::default()
        };
        text_edit_clipboard_shortcuts(&ctx, &mut input);
        assert!(matches!(
            input.events.as_slice(),
            [egui::Event::Copy, egui::Event::Cut]
        ));
        let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
        assert!(output.viewport_output.values().any(|viewport| {
            viewport
                .commands
                .iter()
                .any(|cmd| matches!(cmd, egui::ViewportCommand::RequestPaste))
        }));
        output.textures_delta.clear();
    }

    #[test]
    fn terminal_right_click_opens_menu_without_pasting() {
        let (ctx, mut tab) = setup();
        let pos = egui::pos2(400.0, 300.0);
        let click = |pressed| egui::Event::PointerButton {
            pos,
            pressed,
            button: egui::PointerButton::Secondary,
            modifiers: egui::Modifiers::NONE,
        };
        frame(
            &ctx,
            &mut tab,
            vec![egui::Event::PointerMoved(pos), click(true)],
            true,
        );
        let output = frame_config(&ctx, &mut tab, vec![click(false)], true, &Config::default());
        assert!(egui::Popup::is_any_open(&ctx));
        assert!(!output.viewport_output.values().any(|viewport| {
            viewport
                .commands
                .iter()
                .any(|command| matches!(command, egui::ViewportCommand::RequestPaste))
        }));
    }

    #[test]
    fn terminal_scrollbar_reaches_history_without_selecting_text() {
        let (ctx, mut tab) = setup();
        tab.terminal.lock().unwrap().feed(&b"line\r\n".repeat(100));
        frame(&ctx, &mut tab, vec![], true);
        let pos = egui::pos2(780.0, 60.0);
        let click = |pressed| egui::Event::PointerButton {
            pos,
            pressed,
            button: egui::PointerButton::Primary,
            modifiers: egui::Modifiers::NONE,
        };
        frame(
            &ctx,
            &mut tab,
            vec![egui::Event::PointerMoved(pos), click(true)],
            true,
        );
        frame(&ctx, &mut tab, vec![click(false)], true);
        let terminal = tab.terminal.lock().unwrap();
        assert!(terminal.snapshot().scroll_offset > 0);
        assert!(terminal.selection_text().is_empty());
    }

    #[test]
    fn paste_trims_only_tail_and_can_be_disabled() {
        let (ctx, mut tab) = setup();
        frame(
            &ctx,
            &mut tab,
            vec![egui::Event::Paste("  echo hello \t\r\n".into())],
            true,
        );
        assert_eq!(written(&tab), b"  echo hello");
        assert!(tab.pending_paste.is_none());
        tab.test_sent.clear();
        frame(
            &ctx,
            &mut tab,
            vec![egui::Event::Paste(" \t\r\n".into())],
            true,
        );
        assert!(written(&tab).is_empty());
        let config = Config {
            trim_paste: false,
            ..Default::default()
        };
        frame_config(
            &ctx,
            &mut tab,
            vec![egui::Event::Paste("echo hello \t".into())],
            true,
            &config,
        );
        assert_eq!(written(&tab), b"echo hello \t");
        frame_config(
            &ctx,
            &mut tab,
            vec![egui::Event::Paste("echo hello\n".into())],
            true,
            &config,
        );
        assert_eq!(tab.pending_paste.as_deref(), Some("echo hello\n"));
    }

    #[test]
    fn drag_selection_copies_on_release_and_respects_setting() {
        for enabled in [true, false] {
            for reverse in [true, false] {
                let (ctx, mut tab) = setup();
                tab.terminal.lock().unwrap().feed(b"abcdefghij");
                let config = Config {
                    copy_on_select: enabled,
                    ..Default::default()
                };
                // CentralPanel margin (8) + terminal padding (8).
                let width = ctx.fonts_mut(|f| f.glyph_width(&egui::FontId::monospace(16.0), 'M'));
                let a = egui::pos2(16.0 + width * 1.1, 20.0);
                let b = egui::pos2(16.0 + width * 5.1, 20.0);
                let (start, end) = if reverse { (b, a) } else { (a, b) };
                let button = |pos, pressed| egui::Event::PointerButton {
                    pos,
                    pressed,
                    button: egui::PointerButton::Primary,
                    modifiers: egui::Modifiers::NONE,
                };
                frame_config(
                    &ctx,
                    &mut tab,
                    vec![egui::Event::PointerMoved(start), button(start, true)],
                    true,
                    &config,
                );
                frame_config(
                    &ctx,
                    &mut tab,
                    vec![egui::Event::PointerMoved(end)],
                    true,
                    &config,
                );
                let output = frame_config(&ctx, &mut tab, vec![button(end, false)], true, &config);
                assert_eq!(tab.terminal.lock().unwrap().selection_text(), "bcde");
                let copied: Vec<_> = output
                    .platform_output
                    .commands
                    .iter()
                    .filter_map(|command| match command {
                        egui::OutputCommand::CopyText(text) => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                assert_eq!(copied, if enabled { vec!["bcde"] } else { vec![] });
            }
        }
    }

    #[test]
    fn close_search_button_hides_bar_and_restores_terminal_input() {
        let (ctx, mut tab) = setup();
        tab.show_search = true;
        tab.search = "keep query".into();
        let output = frame_config(&ctx, &mut tab, vec![], true, &Config::default());
        let close = output
            .shapes
            .iter()
            .find_map(|shape| crate::composer_icons::find_text(&shape.shape, "关闭查找"))
            .expect("search must have a visible close button")
            .visual_bounding_rect()
            .center();
        for pressed in [true, false] {
            frame(
                &ctx,
                &mut tab,
                vec![
                    egui::Event::PointerMoved(close),
                    egui::Event::PointerButton {
                        pos: close,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                true,
            );
        }
        assert!(!tab.show_search);
        assert_eq!(tab.search, "keep query");
        tab.test_sent.clear();
        frame(&ctx, &mut tab, vec![egui::Event::Text("x".into())], true);
        assert_eq!(written(&tab), b"x");
    }

    #[test]
    fn star_input_does_not_wait_for_output_parser() {
        let (ctx, mut tab) = setup();
        let terminal = tab.terminal.clone();
        let guard = terminal.lock().unwrap();
        tab.test_sent.clear();
        frame(
            &ctx,
            &mut tab,
            vec![
                egui::Event::Key {
                    key: egui::Key::Num8,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::SHIFT,
                },
                egui::Event::Text("*".into()),
            ],
            true,
        );
        assert_eq!(written(&tab), b"*");
        assert!(tab.pending_bottom);
        drop(guard);
        frame(&ctx, &mut tab, vec![], true);
        assert!(!tab.pending_bottom);
    }

    #[test]
    fn pointer_return_restores_terminal_focus_but_respects_input_block() {
        let (ctx, mut tab) = setup();
        for allow in [false, true] {
            frame(
                &ctx,
                &mut tab,
                vec![egui::Event::PointerMoved(egui::pos2(0.0, 0.0))],
                true,
            );
            let focused = ctx.memory(|memory| memory.focused()).unwrap();
            ctx.memory_mut(|memory| memory.surrender_focus(focused));
            tab.focus = false;
            tab.test_sent.clear();
            frame(
                &ctx,
                &mut tab,
                vec![
                    egui::Event::PointerMoved(egui::pos2(400.0, 300.0)),
                    egui::Event::Text("*".into()),
                ],
                allow,
            );
            assert_eq!(written(&tab), if allow { b"*".as_slice() } else { b"" });
            tab.focus = true;
            frame(&ctx, &mut tab, vec![], true);
        }
    }

    #[test]
    fn shell_editing_sequences_and_search_are_distinct() {
        let (ctx, mut tab) = setup();
        let modified = |key, modifiers| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        for (key, expected) in [
            (egui::Key::A, 1),
            (egui::Key::B, 2),
            (egui::Key::D, 4),
            (egui::Key::E, 5),
            (egui::Key::F, 6),
            (egui::Key::G, 7),
            (egui::Key::H, 8),
            (egui::Key::K, 11),
            (egui::Key::L, 12),
            (egui::Key::N, 14),
            (egui::Key::O, 15),
            (egui::Key::P, 16),
            (egui::Key::Q, 17),
            (egui::Key::R, 18),
            (egui::Key::S, 19),
            (egui::Key::T, 20),
            (egui::Key::U, 21),
            (egui::Key::W, 23),
            (egui::Key::Y, 25),
            (egui::Key::Z, 26),
        ] {
            tab.test_sent.clear();
            frame(
                &ctx,
                &mut tab,
                vec![modified(key, egui::Modifiers::CTRL)],
                true,
            );
            assert_eq!(written(&tab), vec![expected], "{key:?}");
            assert!(!tab.show_search);
        }
        tab.test_sent.clear();
        frame(
            &ctx,
            &mut tab,
            vec![
                egui::Event::Cut,
                egui::Event::Cut,
                modified(
                    egui::Key::Minus,
                    egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                ),
                modified(egui::Key::ArrowLeft, egui::Modifiers::CTRL),
                modified(egui::Key::ArrowRight, egui::Modifiers::CTRL),
                key(egui::Key::Escape),
                egui::Event::Text("3b".into()),
            ],
            true,
        );
        assert_eq!(written(&tab), b"\x18\x18\x1f\x1b[1;5D\x1b[1;5C\x1b3b");
        tab.test_sent.clear();
        frame(
            &ctx,
            &mut tab,
            vec![modified(
                egui::Key::F,
                egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
            )],
            true,
        );
        assert!(tab.show_search);
        assert!(written(&tab).is_empty());
    }

    #[test]
    fn alt_editing_works_with_or_without_text_events_without_duplicates() {
        for (key, text, expected) in [
            (egui::Key::B, "b", b"\x1bb".as_slice()),
            (egui::Key::F, "f", b"\x1bf"),
            (egui::Key::D, "d", b"\x1bd"),
            (egui::Key::T, "t", b"\x1bt"),
            (egui::Key::R, "r", b"\x1br"),
            (egui::Key::Period, ".", b"\x1b."),
            (egui::Key::Num3, "3", b"\x1b3"),
            (egui::Key::Backspace, "", b"\x1b\x7f"),
        ] {
            for with_text in [false, true] {
                let (ctx, mut tab) = setup();
                let mut events = vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::ALT,
                }];
                if with_text && !text.is_empty() {
                    events.push(egui::Event::Text(text.into()));
                }
                frame(&ctx, &mut tab, events, true);
                assert_eq!(written(&tab), expected, "{key:?}, text={with_text}");
            }
        }
    }

    #[test]
    fn focus_change_clears_stale_ime_and_restores_ssh_control_keys() {
        let (ctx, mut tab) = setup();
        tab.preedit = "unfinished".into();
        frame(&ctx, &mut tab, vec![], false);
        for (key, byte) in [
            (egui::Key::A, 1),
            (egui::Key::E, 5),
            (egui::Key::U, 21),
            (egui::Key::K, 11),
            (egui::Key::W, 23),
            (egui::Key::R, 18),
            (egui::Key::L, 12),
            (egui::Key::D, 4),
            (egui::Key::Z, 26),
        ] {
            tab.test_sent.clear();
            frame(
                &ctx,
                &mut tab,
                vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::CTRL,
                }],
                true,
            );
            assert_eq!(written(&tab), vec![byte], "{key:?}");
        }
    }

    #[test]
    fn history_shortcuts_do_not_send_commands() {
        let (ctx, mut tab) = setup();
        tab.terminal.lock().unwrap().feed(&b"line\r\n".repeat(100));
        for (key, at_top) in [(egui::Key::Home, true), (egui::Key::End, false)] {
            frame(
                &ctx,
                &mut tab,
                vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::SHIFT,
                }],
                true,
            );
            let snapshot = tab.terminal.lock().unwrap().snapshot();
            assert_eq!(
                snapshot.scroll_offset,
                if at_top { snapshot.history_size } else { 0 }
            );
            assert!(written(&tab).is_empty());
        }
    }

    #[test]
    fn ctrl_c_sends_interrupt_instead_of_swallowing_it() {
        let (ctx, mut tab) = setup();
        frame(&ctx, &mut tab, vec![egui::Event::Copy], true);
        assert_eq!(written(&tab), vec![3]);
    }
}

#[cfg(test)]
mod tab_tests {
    use super::*;
    fn tabs() -> Vec<Tab> {
        (0..4)
            .map(|index| {
                let mut tab = App::make_tab(Profile::default());
                tab.local = index % 2 == 0;
                tab
            })
            .collect()
    }
    #[test]
    fn closing_background_tabs_keeps_active_identity() {
        let mut tabs = tabs();
        let selected = tabs[2].id;
        let mut active = 2;
        close_tabs(&mut tabs, &mut active, 0, CloseTabs::Current);
        assert_eq!(active, 1);
        assert_eq!(tabs[active].id, selected);
        close_tabs(&mut tabs, &mut active, 1, CloseTabs::Right);
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs[active].id, selected);
    }
    #[test]
    fn connecting_closes_only_startup_local_tab() {
        let mut tabs = tabs();
        let startup = tabs[0].id;
        let manual_local = tabs[2].id;
        let selected = tabs[3].id;
        let mut active = 3;
        close_startup_local_tab(&mut tabs, &mut active, startup);
        assert_eq!(tabs.len(), 3);
        assert_eq!(tabs[active].id, selected);
        assert!(tabs.iter().any(|tab| tab.id == manual_local));
    }
    #[test]
    fn bulk_close_targets_local_others_and_all() {
        let mut tabs = tabs();
        let target = tabs[1].id;
        let mut active = 3;
        close_tabs(&mut tabs, &mut active, 1, CloseTabs::Local);
        assert_eq!(tabs.len(), 2);
        assert!(tabs.iter().all(|tab| !tab.local));
        assert_eq!(active, 1);
        close_tabs(&mut tabs, &mut active, 0, CloseTabs::Others);
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[active].id, target);
        close_tabs(&mut tabs, &mut active, 0, CloseTabs::All);
        assert!(tabs.is_empty());
        assert_eq!(active, 0);
    }

    #[test]
    fn close_disconnected_keeps_live_and_local_tabs() {
        let mut tabs = tabs();
        tabs[0].finished = true;
        tabs[1].finished = true;
        tabs[3].connected = true;
        tabs[3].finished = true;
        let selected = tabs[2].id;
        let mut active = 2;
        close_tabs(&mut tabs, &mut active, 0, CloseTabs::Disconnected);
        assert_eq!(tabs.len(), 3);
        assert_eq!(tabs[active].id, selected);
        assert!(tabs.iter().any(|tab| tab.id == selected));
        assert!(tabs.iter().any(|tab| tab.connected));
        assert!(tabs.iter().any(|tab| tab.local));
    }
}
fn serial_fields(ui: &mut egui::Ui, editor: &mut Editor, ctx: &egui::Context) {
    ui.label("端口号");
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut editor.profile.serial.port_name).desired_width(90.0),
        );
        egui::ComboBox::from_id_salt("serial_ports")
            .selected_text("可用端口")
            .show_ui(ui, |ui| {
                for name in &editor.serial_ports {
                    ui.selectable_value(&mut editor.profile.serial.port_name, name.clone(), name);
                }
                if editor.serial_ports.is_empty() {
                    ui.label("未发现端口，可手动填写或刷新");
                }
            });
        if ui
            .add_enabled(editor.port_scan.is_none(), egui::Button::new("刷新"))
            .clicked()
        {
            let (tx, rx) = std::sync::mpsc::channel();
            let notify = ctx.clone();
            std::thread::spawn(move || {
                let _ =
                    tx.send(rshell_core::serial::available_ports().map_err(|e| format!("{e:#}")));
                notify.request_repaint();
            });
            editor.port_scan = Some(rx);
        }
    });
    ui.end_row();
    if !editor.port_error.is_empty() {
        ui.label("端口检测");
        ui.label(&editor.port_error);
        ui.end_row();
    }
    let settings = &mut editor.profile.serial;
    ui.label("波特率");
    ui.horizontal(|ui| {
        ui.add(egui::DragValue::new(&mut settings.baud_rate).range(1..=u32::MAX));
        egui::ComboBox::from_id_salt("baud_rate")
            .selected_text("常用值")
            .show_ui(ui, |ui| {
                for rate in [
                    110, 300, 600, 1200, 2400, 4800, 9600, 14400, 19200, 38400, 56000, 57600,
                    115200, 128000, 230400, 256000, 460800, 921600,
                ] {
                    ui.selectable_value(&mut settings.baud_rate, rate, rate.to_string());
                }
            });
    });
    ui.end_row();
    ui.label("数据位");
    egui::ComboBox::from_id_salt("data_bits")
        .selected_text(settings.data_bits.to_string())
        .show_ui(ui, |ui| {
            for bits in 5..=8 {
                ui.selectable_value(&mut settings.data_bits, bits, bits.to_string());
            }
        });
    ui.end_row();
    ui.label("停止位");
    egui::ComboBox::from_id_salt("stop_bits")
        .selected_text(settings.stop_bits.to_string())
        .show_ui(ui, |ui| {
            for bits in [1, 2] {
                ui.selectable_value(&mut settings.stop_bits, bits, bits.to_string());
            }
        });
    ui.end_row();
    ui.label("奇偶校验");
    let parity_name = |p| match p {
        SerialParity::None => "无 (None)",
        SerialParity::Odd => "奇 (Odd)",
        SerialParity::Even => "偶 (Even)",
    };
    egui::ComboBox::from_id_salt("parity")
        .selected_text(parity_name(settings.parity))
        .show_ui(ui, |ui| {
            for parity in [SerialParity::None, SerialParity::Odd, SerialParity::Even] {
                ui.selectable_value(&mut settings.parity, parity, parity_name(parity));
            }
        });
    ui.end_row();
    ui.label("流控制");
    let flow_name = |f| match f {
        SerialFlowControl::None => "无 (None)",
        SerialFlowControl::Software => "XON/XOFF",
        SerialFlowControl::Hardware => "RTS/CTS",
    };
    egui::ComboBox::from_id_salt("flow_control")
        .selected_text(flow_name(settings.flow_control))
        .show_ui(ui, |ui| {
            for flow in [
                SerialFlowControl::None,
                SerialFlowControl::Software,
                SerialFlowControl::Hardware,
            ] {
                ui.selectable_value(&mut settings.flow_control, flow, flow_name(flow));
            }
        });
    ui.end_row();
}
