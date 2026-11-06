use crate::composer_icons::{self, Icon};
use eframe::egui::{self, Color32};
use rshell_core::{
    config::Config,
    session_tree::{Item, Placement},
    store::Action,
};

pub(super) fn folder_row(ui: &mut egui::Ui, name: &str, open: bool, zoom: f32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 32.0 * zoom),
        egui::Sense::click_and_drag(),
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), name));
    let painter = ui.painter_at(rect);
    if response.hovered() || response.dragged() {
        painter.rect_filled(rect, 3.0, ui.visuals().widgets.hovered.weak_bg_fill);
    }
    let icon_rect = |x: f32, size: f32| {
        egui::Rect::from_center_size(
            egui::pos2(rect.left() + (x + size / 2.0) * zoom, rect.center().y),
            egui::vec2(size * zoom, size * zoom),
        )
    };
    composer_icons::image(
        if open {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        },
        ui.visuals().weak_text_color(),
    )
    .paint_at(ui, icon_rect(2.0, 12.0));
    let folder_color = if ui.visuals().dark_mode {
        Color32::from_rgb(224, 183, 98)
    } else {
        Color32::from_rgb(153, 109, 30)
    };
    composer_icons::image(
        if open { Icon::FolderOpen } else { Icon::Folder },
        folder_color,
    )
    .paint_at(ui, icon_rect(20.0, 14.0));
    let text_start = rect.left() + 42.0 * zoom;
    let mut job = egui::text::LayoutJob::simple_singleline(
        name.into(),
        egui::FontId::proportional(15.0 * zoom),
        ui.visuals().text_color(),
    );
    job.wrap.max_width = (rect.right() - 22.0 * zoom - text_start).max(0.0);
    job.wrap.max_rows = 1;
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    painter.galley(
        egui::pos2(text_start, rect.center().y - galley.size().y / 2.0),
        galley,
        ui.visuals().text_color(),
    );
    if response.hovered() || response.dragged() {
        composer_icons::image(Icon::GripVertical, ui.visuals().weak_text_color()).paint_at(
            ui,
            egui::Rect::from_center_size(
                egui::pos2(rect.right() - 10.0 * zoom, rect.center().y),
                egui::vec2(12.0, 12.0) * zoom,
            ),
        );
    }
    response.on_hover_text(format!("{name}\n拖动上/下边缘排序；拖到文件夹中部移入"))
}

pub(super) fn placement(rect: egui::Rect, y: f32, target: Item) -> Placement {
    if matches!(target, Item::Folder(_)) {
        if y < rect.top() + rect.height() * 0.25 {
            Placement::Before(target)
        } else if y > rect.bottom() - rect.height() * 0.25 {
            Placement::After(target)
        } else if let Item::Folder(id) = target {
            Placement::Into(Some(id))
        } else {
            unreachable!()
        }
    } else if y < rect.center().y {
        Placement::Before(target)
    } else {
        Placement::After(target)
    }
}

pub(super) fn drop_target(
    ui: &mut egui::Ui,
    response: &egui::Response,
    config: &Config,
    target: Option<Item>,
) -> Option<Action> {
    let payload = response.dnd_hover_payload::<Item>()?;
    let destination = target.map_or(Placement::Into(None), |target| {
        placement(
            response.rect,
            ui.input(|i| i.pointer.hover_pos())
                .unwrap_or(response.rect.center())
                .y,
            target,
        )
    });
    if !matches!(config.moved_session(*payload, destination), Ok(Some(_))) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::NotAllowed);
        return None;
    }
    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    let stroke = egui::Stroke::new(2.0, ui.visuals().selection.stroke.color);
    match destination {
        Placement::Into(_) => {
            ui.painter().rect_stroke(
                response.rect.shrink(1.0),
                3.0,
                stroke,
                egui::StrokeKind::Inside,
            );
        }
        Placement::Before(_) | Placement::After(_) => {
            let y = if matches!(destination, Placement::Before(_)) {
                response.rect.top()
            } else {
                response.rect.bottom()
            };
            ui.painter().line_segment(
                [
                    egui::pos2(response.rect.left(), y),
                    egui::pos2(response.rect.right(), y),
                ],
                stroke,
            );
            ui.painter().circle_filled(
                egui::pos2(response.rect.left() + 2.0, y),
                3.0,
                stroke.color,
            );
        }
    }
    response.dnd_release_payload::<Item>()?;
    Some(Action::MoveSession {
        item: *payload,
        placement: destination,
    })
}

pub(super) fn auto_scroll(ui: &egui::Ui) {
    if !egui::DragAndDrop::has_payload_of_type::<Item>(ui.ctx()) {
        return;
    }
    if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
        let rect = ui.clip_rect();
        if rect.contains(pos) {
            let direction = if pos.y < rect.top() + 24.0 {
                1.0
            } else if pos.y > rect.bottom() - 24.0 {
                -1.0
            } else {
                0.0
            };
            if direction != 0.0 {
                ui.scroll_with_delta(egui::vec2(
                    0.0,
                    direction * 240.0 * ui.input(|i| i.stable_dt).min(0.05),
                ));
                ui.ctx().request_repaint();
            }
        }
    }
}

pub(super) fn smoke_fixture(config: &mut Config) {
    use rshell_core::config::{Folder, Profile};
    use uuid::Uuid;
    let root = Folder {
        id: Uuid::new_v4(),
        name: "测试会话".into(),
        parent_id: None,
    };
    let a = Folder {
        id: Uuid::new_v4(),
        name: "应用服务".into(),
        parent_id: Some(root.id),
    };
    let b = Folder {
        id: Uuid::new_v4(),
        name: "数据库与测试环境长名称".into(),
        parent_id: Some(root.id),
    };
    config.profiles = vec![
        Profile {
            name: "本地测试 A".into(),
            host: "127.0.0.1".into(),
            folder_id: Some(a.id),
            ..Default::default()
        },
        Profile {
            name: "本地测试 B".into(),
            host: "127.0.0.1".into(),
            folder_id: Some(a.id),
            ..Default::default()
        },
        Profile {
            name: "只读演示".into(),
            host: "127.0.0.1".into(),
            folder_id: Some(b.id),
            ..Default::default()
        },
    ];
    config.folders = vec![root, a, b];
    config.session_order.clear();
}

#[cfg(test)]
mod tests {
    use super::super::App;
    use super::*;
    use rshell_core::config::{Folder, Profile};
    use std::{
        path::PathBuf,
        time::{Duration, Instant},
    };
    use uuid::Uuid;

    struct Harness {
        app: App,
        ctx: egui::Context,
        _temp: tempfile::TempDir,
    }

    impl Harness {
        fn new(dark: bool) -> Self {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target");
            let temp = tempfile::tempdir_in(root).unwrap();
            let ctx = egui::Context::default();
            let a = Folder {
                id: Uuid::new_v4(),
                name: "Folder A".into(),
                parent_id: None,
            };
            let b = Folder {
                id: Uuid::new_v4(),
                name: "Folder B".into(),
                parent_id: None,
            };
            let nested = Folder {
                id: Uuid::new_v4(),
                name: "Nested".into(),
                parent_id: Some(a.id),
            };
            let config = Config {
                light_theme: !dark,
                folders: vec![a, b, nested],
                profiles: vec![
                    Profile {
                        name: "keep alpha".into(),
                        host: "127.0.0.1".into(),
                        ..Default::default()
                    },
                    Profile {
                        name: "keep beta".into(),
                        host: "127.0.0.1".into(),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            };
            let cc = eframe::CreationContext::_new_kittest(ctx.clone());
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .unwrap();
            let app = App::new(
                &cc,
                config,
                temp.path().join("config.json"),
                runtime,
                true,
                false,
            )
            .unwrap();
            ctx.all_styles_mut(|s| s.animation_time = 0.0);
            Self {
                app,
                ctx,
                _temp: temp,
            }
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
            self.app.poll_store(&self.ctx);
            let mut output = self.ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(180.0, 700.0),
                    )),
                    focused: true,
                    events,
                    ..Default::default()
                },
                |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let root = egui::CollapsingHeader::new("All")
                        .default_open(true)
                        .show(ui, |ui| self.app.folder_tree(ui, None));
                    if let Some(action) =
                        drop_target(ui, &root.header_response, &self.app.config, None)
                    {
                        self.app.submit(action, super::super::Pending::Update);
                    }
                },
            );
            output.textures_delta.clear();
            output
        }

        fn position(&mut self, name: &str) -> egui::Pos2 {
            for _ in 0..3 {
                self.frame(vec![]);
            }
            self.frame(vec![])
                .shapes
                .iter()
                .find_map(|s| {
                    composer_icons::find_text(&s.shape, name)
                        .map(|t| t.visual_bounding_rect().center())
                })
                .unwrap_or_else(|| panic!("missing row: {name}"))
        }

        fn drag(&mut self, from: &str, to: &str, dy: f32, cancel: bool) {
            let start = self.position(from);
            let end = self.position(to) + egui::vec2(0.0, dy);
            let button = |pos, pressed| egui::Event::PointerButton {
                pos,
                pressed,
                button: egui::PointerButton::Primary,
                modifiers: egui::Modifiers::NONE,
            };
            self.frame(vec![egui::Event::PointerMoved(start)]);
            self.frame(vec![button(start, true)]);
            self.frame(vec![egui::Event::PointerMoved(end)]);
            self.frame(vec![]);
            if cancel {
                self.frame(vec![egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }]);
            }
            self.frame(vec![button(end, false)]);
            let deadline = Instant::now() + Duration::from_secs(3);
            while !self.app.pending.is_empty() {
                self.app.poll_store(&self.ctx);
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(self.app.tabs.len(), 1, "drag must not connect a session");
            assert!(self.app.editor.is_none(), "drag must not prompt for login");
        }
    }

    #[test]
    fn pointer_drag_reorders_moves_subtrees_and_persists_in_both_themes() {
        for dark in [true, false] {
            let mut h = Harness::new(dark);
            let a = Item::Folder(h.app.config.folders[0].id);
            let b = Item::Folder(h.app.config.folders[1].id);
            let nested_id = h.app.config.folders[2].id;
            let p = Item::Profile(h.app.config.profiles[0].id);
            let q = Item::Profile(h.app.config.profiles[1].id);
            h.drag("Folder B", "Folder A", -14.0, false);
            assert_eq!(h.app.config.session_children(None), vec![b, a, p, q]);
            h.drag("keep beta", "keep alpha", -8.0, false);
            assert_eq!(h.app.config.session_children(None), vec![b, a, q, p]);
            h.drag("keep alpha", "Folder B", -14.0, false);
            assert_eq!(h.app.config.session_children(None), vec![p, b, a, q]);
            h.drag("keep beta", "Nested", 0.0, false);
            assert_eq!(h.app.config.session_children(Some(nested_id)), vec![q]);
            h.drag("Folder B", "Folder A", 0.0, false);
            assert_eq!(
                h.app.config.session_parent(b).unwrap(),
                h.app
                    .config
                    .session_parent(Item::Folder(nested_id))
                    .unwrap()
            );
            h.drag("Folder B", "All", 0.0, false);
            assert_eq!(h.app.config.session_children(None), vec![p, a, b]);
            h.drag("keep beta", "All", 0.0, false);
            assert_eq!(h.app.config.session_children(None), vec![p, a, b, q]);
            let disk = Config::load(&h._temp.path().join("config.json")).unwrap();
            assert_eq!(disk.session_children(None), vec![p, a, b, q]);
            h.app.filter = "keep".into();
            h.drag("keep beta", "keep alpha", -8.0, false);
            assert_eq!(h.app.config.session_children(None), vec![q, p, a, b]);
        }
    }

    #[test]
    fn pointer_drag_cancel_and_invalid_descendant_do_not_change_tree() {
        let mut h = Harness::new(true);
        let before = serde_json::to_value(&h.app.config).unwrap();
        h.drag("Folder B", "Folder A", -14.0, true);
        h.drag("Folder A", "Nested", 0.0, false);
        assert_eq!(serde_json::to_value(&h.app.config).unwrap(), before);
    }
}
