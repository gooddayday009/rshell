use super::*;

#[derive(Default)]
struct PaneGroup {
    tabs: Vec<Uuid>,
    selected: Option<Uuid>,
}

#[derive(Default)]
pub(super) struct PaneGroups {
    groups: Vec<PaneGroup>,
    focused: usize,
}

impl PaneGroups {
    pub(super) fn sync(&mut self, tabs: &[Tab], active: &mut usize, layout: TerminalLayout) {
        let count = if layout == TerminalLayout::Grid { 4 } else { 2 };
        if self.groups.is_empty() {
            self.groups.resize_with(count, PaneGroup::default);
            for (index, tab) in tabs.iter().enumerate() {
                self.groups[index % count].tabs.push(tab.id);
            }
        }
        if self.groups.len() > count {
            let removed = self.groups.split_off(count);
            for group in removed {
                self.groups[count - 1].tabs.extend(group.tabs);
            }
        }
        self.groups.resize_with(count, PaneGroup::default);
        self.focused = self.focused.min(count - 1);
        let incoming_is_new = tabs
            .get(*active)
            .is_some_and(|tab| !self.groups.iter().any(|group| group.tabs.contains(&tab.id)));
        let focused_selection_removed = self.groups[self.focused]
            .selected
            .is_some_and(|id| !tabs.iter().any(|tab| tab.id == id));
        for group in &mut self.groups {
            let old_position = group
                .tabs
                .iter()
                .position(|id| Some(*id) == group.selected)
                .unwrap_or(0);
            group.tabs.retain(|id| tabs.iter().any(|tab| tab.id == *id));
            if !group.tabs.iter().any(|id| Some(*id) == group.selected) {
                group.selected = group
                    .tabs
                    .get(old_position.min(group.tabs.len().saturating_sub(1)))
                    .copied();
            }
        }
        for tab in tabs {
            if !self.groups.iter().any(|group| group.tabs.contains(&tab.id)) {
                self.groups[self.focused].tabs.push(tab.id);
            }
        }
        if focused_selection_removed && !incoming_is_new {
            self.activate(tabs, active, self.focused);
        } else if let Some(tab) = tabs.get(*active)
            && let Some(group) = self
                .groups
                .iter()
                .position(|group| group.tabs.contains(&tab.id))
        {
            self.focused = group;
            self.groups[group].selected = Some(tab.id);
        }
        // A newly populated previously empty group always has a visible tab.
        for group in &mut self.groups {
            if group.selected.is_none() {
                group.selected = group.tabs.first().copied();
            }
        }
    }

    fn activate(&mut self, tabs: &[Tab], active: &mut usize, group: usize) {
        self.focused = group;
        *active = tabs
            .iter()
            .position(|tab| Some(tab.id) == self.groups[group].selected)
            .unwrap_or(tabs.len());
    }

    pub(super) fn switch(&mut self, tabs: &[Tab], active: &mut usize, direction: isize) {
        let group = &mut self.groups[self.focused];
        let index = group
            .tabs
            .iter()
            .position(|id| Some(*id) == group.selected)
            .unwrap_or(0);
        group.selected = group
            .tabs
            .get(adjacent_tab(index, group.tabs.len(), direction))
            .copied();
        self.activate(tabs, active, self.focused);
    }

    fn move_tab(&mut self, id: Uuid, destination: usize, tabs: &[Tab], active: &mut usize) {
        let Some(source) = self
            .groups
            .iter()
            .position(|group| group.tabs.contains(&id))
        else {
            return;
        };
        if source != destination {
            let group = &mut self.groups[source];
            let index = group.tabs.iter().position(|tab| *tab == id).unwrap();
            group.tabs.remove(index);
            if group.selected == Some(id) {
                group.selected = group
                    .tabs
                    .get(index.min(group.tabs.len().saturating_sub(1)))
                    .copied();
            }
            self.groups[destination].tabs.push(id);
        }
        self.groups[destination].selected = Some(id);
        self.activate(tabs, active, destination);
    }
}

struct PaneTabDrag(Uuid);

#[derive(Default)]
pub(super) struct PaneResponse {
    pub selection: Option<(Uuid, String)>,
    pub action: Option<(usize, TabAction)>,
    pub new_local: bool,
}

pub(super) fn terminal_panes(
    ui: &mut egui::Ui,
    tabs: &mut [Tab],
    active: &mut usize,
    config: &Config,
    layout: TerminalLayout,
    groups: &mut PaneGroups,
    allow_input: bool,
) -> PaneResponse {
    groups.sync(tabs, active, layout);
    let (rects, resizing) = layout.rects(ui, groups.groups.len(), allow_input);
    let mut result = PaneResponse::default();
    let dragging_tab = egui::DragAndDrop::payload::<PaneTabDrag>(ui.ctx()).is_some();
    let allow_input = allow_input && !resizing;
    // Choose the hovered pane before drawing any terminal: otherwise text in
    // the same frame can still be delivered to the previously focused session.
    if allow_input
        && !dragging_tab
        && !egui::Popup::is_any_open(ui.ctx())
        && !tabs.iter().any(|tab| tab.pending_paste.is_some())
        && ui.input(|input| !input.pointer.any_down())
        && let Some(group) = rects.iter().position(|rect| {
            let body = egui::Rect::from_min_max(
                egui::pos2(rect.left() + 2.0, (rect.top() + 32.0).min(rect.bottom())),
                egui::pos2(rect.right() - 2.0, rect.bottom()),
            );
            ui.rect_contains_pointer(body)
        })
        && group != groups.focused
    {
        groups.activate(tabs, active, group);
        if let Some(tab) = tabs.get_mut(*active) {
            tab.focus = true;
        }
    }
    if allow_input
        && !dragging_tab
        && ui.input(|input| input.pointer.any_pressed())
        && let Some(pos) = ui.input(|input| input.pointer.interact_pos())
        && let Some(group) = rects.iter().position(|rect| rect.contains(pos))
    {
        groups.activate(tabs, active, group);
        if let Some(tab) = tabs.get_mut(*active) {
            tab.focus = true;
        }
    }

    let mut move_tab = None;
    // Resolve every tab-bar interaction before rendering any terminal, so a
    // click and text in the same frame cannot reach the previous pane as well.
    for (pane, rect) in rects.iter().copied().enumerate() {
        let header = egui::Rect::from_min_max(
            rect.min,
            egui::pos2(rect.right(), (rect.top() + 30.0).min(rect.bottom())),
        );
        ui.scope_builder(
            egui::UiBuilder::new()
                .id_salt(("pane_header", pane))
                .max_rect(header),
            |ui| {
                ui.set_clip_rect(header);
                ui.add_enabled_ui(allow_input, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        let strip_width = (header.width() - 32.0).max(1.0);
                        egui::ScrollArea::horizontal()
                            .id_salt("tabs")
                            .max_width(strip_width)
                            .auto_shrink([false, true])
                            .scroll_bar_visibility(
                                egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                            )
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    let ids = groups.groups[pane].tabs.clone();
                                    let width = (strip_width / ids.len().max(1) as f32 - 24.0)
                                        .clamp(64.0, 140.0);
                                    for id in ids {
                                        let Some(index) = tabs.iter().position(|tab| tab.id == id)
                                        else {
                                            continue;
                                        };
                                        let tab = &mut tabs[index];
                                        let selected = groups.groups[pane].selected == Some(id);
                                        let response = ui
                                            .add_sized(
                                                [width, 24.0],
                                                egui::Button::new(&tab.title)
                                                    .selected(selected)
                                                    .truncate()
                                                    .sense(egui::Sense::click_and_drag()),
                                            )
                                            .on_hover_text(format!(
                                                "{} · {}\n拖到另一屏移动标签",
                                                tab.title, tab.status
                                            ));
                                        response.dnd_set_drag_payload(PaneTabDrag(id));
                                        if selected && tab.focus {
                                            response.scroll_to_me(None);
                                        }
                                        if response.clicked() {
                                            groups.groups[pane].selected = Some(id);
                                            groups.focused = pane;
                                            *active = index;
                                            tab.focus = true;
                                            response.scroll_to_me(None);
                                        }
                                        response.context_menu(|ui| {
                                            for target in 0..groups.groups.len() {
                                                if target != pane
                                                    && ui
                                                        .button(format!(
                                                            "移动到第 {} 屏",
                                                            target + 1
                                                        ))
                                                        .clicked()
                                                {
                                                    move_tab = Some((id, target));
                                                    ui.close();
                                                }
                                            }
                                            ui.separator();
                                            if ui
                                                .add_enabled(
                                                    !tab.local
                                                        && tab.profile.protocol == Protocol::Ssh
                                                        && tab.connected,
                                                    egui::Button::new("复制 SSH 渠道"),
                                                )
                                                .clicked()
                                            {
                                                groups.focused = pane;
                                                groups.groups[pane].selected = Some(id);
                                                *active = index;
                                                result.action = Some((index, TabAction::Channel));
                                                ui.close();
                                            }
                                            for (label, action) in [
                                                ("复制会话", TabAction::Duplicate),
                                                ("重命名…", TabAction::Rename),
                                                ("清屏", TabAction::ClearScreen),
                                                ("关闭标签", TabAction::Close(CloseTabs::Current)),
                                            ] {
                                                if ui.button(label).clicked() {
                                                    groups.focused = pane;
                                                    groups.groups[pane].selected = Some(id);
                                                    *active = index;
                                                    result.action = Some((index, action));
                                                    ui.close();
                                                }
                                            }
                                        });
                                        if ui
                                            .add_sized([20.0, 24.0], egui::Button::new("×"))
                                            .on_hover_text("关闭此标签")
                                            .clicked()
                                        {
                                            result.action =
                                                Some((index, TabAction::Close(CloseTabs::Current)));
                                        }
                                    }
                                    if groups.groups[pane].tabs.is_empty() {
                                        ui.weak(format!("第 {} 屏 · 空", pane + 1));
                                    }
                                });
                            });
                        if ui
                            .add_sized([26.0, 24.0], egui::Button::new("+"))
                            .on_hover_text(format!(
                                "在此屏新建本地 {}",
                                rshell_core::local::shell_label()
                            ))
                            .clicked()
                        {
                            groups.activate(tabs, active, pane);
                            result.new_local = true;
                        }
                    });
                });
            },
        );
        // Drop onto either the tab strip or the terminal body. This hover-only
        // region does not intercept normal terminal clicks or selection.
        let drop = ui.interact(
            rect,
            ui.scope_id().with(("pane_drop", pane)),
            egui::Sense::hover(),
        );
        if allow_input && drop.dnd_hover_payload::<PaneTabDrag>().is_some() {
            ui.painter().rect_stroke(
                rect,
                0.0,
                ui.visuals().selection.stroke,
                egui::StrokeKind::Inside,
            );
            if let Some(payload) = drop.dnd_release_payload::<PaneTabDrag>() {
                move_tab = Some((payload.0, pane));
            }
        }
    }
    let moved = move_tab.is_some();
    if let Some((id, pane)) = move_tab {
        groups.move_tab(id, pane, tabs, active);
    }
    if moved && let Some(tab) = tabs.get_mut(*active) {
        tab.focus = true;
    }
    let dragging_tab =
        dragging_tab || egui::DragAndDrop::payload::<PaneTabDrag>(ui.ctx()).is_some();
    let active_id = tabs.get(*active).map(|tab| tab.id);
    for tab in tabs.iter_mut() {
        if Some(tab.id) != active_id {
            tab.focus = false;
        }
    }
    for (pane, rect) in rects.into_iter().enumerate() {
        let focused = pane == groups.focused;
        ui.painter().rect_stroke(
            rect,
            0.0,
            if focused {
                ui.visuals().selection.stroke
            } else {
                ui.visuals().widgets.noninteractive.bg_stroke
            },
            egui::StrokeKind::Inside,
        );
        let body = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 2.0, (rect.top() + 32.0).min(rect.bottom())),
            egui::pos2(rect.right() - 2.0, rect.bottom()),
        );
        ui.scope_builder(
            egui::UiBuilder::new()
                .id_salt(("pane_body", pane))
                .max_rect(body),
            |ui| {
                ui.set_clip_rect(body);
                if let Some(index) = tabs
                    .iter()
                    .position(|tab| Some(tab.id) == groups.groups[pane].selected)
                {
                    if let Some(text) = crate::terminal_view::show(
                        ui,
                        &mut tabs[index],
                        config,
                        allow_input
                            && focused
                            && !dragging_tab
                            && !moved
                            && !result.new_local
                            && result.action.is_none(),
                    ) {
                        result.selection = Some((tabs[index].id, text));
                    }
                } else {
                    ui.weak("拖入标签，或点击 + 新建终端");
                }
            },
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tabs(count: usize) -> Vec<Tab> {
        (0..count)
            .map(|index| {
                let mut tab = App::make_tab(Profile::default());
                tab.title = format!("Session {index}");
                tab.connected = true;
                tab
            })
            .collect()
    }

    #[test]
    fn pane_groups_preserve_selections_and_new_move_close_lifecycle() {
        let mut tabs = tabs(4);
        let ids: Vec<_> = tabs.iter().map(|tab| tab.id).collect();
        let mut groups = PaneGroups::default();
        let mut active = 0;
        groups.sync(&tabs, &mut active, TerminalLayout::Columns);
        assert_eq!(groups.groups[0].tabs, vec![ids[0], ids[2]]);
        assert_eq!(groups.groups[1].tabs, vec![ids[1], ids[3]]);
        groups.switch(&tabs, &mut active, 1);
        assert_eq!(active, 2);
        assert_eq!(groups.groups[1].selected, Some(ids[1]));
        groups.activate(&tabs, &mut active, 1);
        groups.switch(&tabs, &mut active, 1);
        assert_eq!(active, 3);
        assert_eq!(groups.groups[0].selected, Some(ids[2]));
        tabs.push(App::make_tab(Profile::default()));
        active = 4;
        groups.sync(&tabs, &mut active, TerminalLayout::Rows);
        let added = tabs[4].id;
        assert!(groups.groups[1].tabs.contains(&added));
        assert_eq!(groups.groups[1].selected, Some(added));
        groups.move_tab(ids[2], 1, &tabs, &mut active);
        assert_eq!(groups.groups[0].selected, Some(ids[0]));
        assert_eq!(groups.groups[1].selected, Some(ids[2]));
        close_tabs(&mut tabs, &mut active, 2, CloseTabs::Current);
        groups.sync(&tabs, &mut active, TerminalLayout::Columns);
        assert_eq!(tabs[active].id, added);
        assert_eq!(groups.groups[0].selected, Some(ids[0]));
        groups.sync(&tabs, &mut active, TerminalLayout::Grid);
        groups.activate(&tabs, &mut active, 3);
        assert_eq!(active, tabs.len());
        tabs.push(App::make_tab(Profile::default()));
        active = tabs.len() - 1;
        let new = tabs[active].id;
        groups.sync(&tabs, &mut active, TerminalLayout::Grid);
        assert_eq!(groups.groups[3].selected, Some(new));
        groups.sync(&tabs, &mut active, TerminalLayout::Columns);
        assert!(groups.groups[1].tabs.contains(&new));
        let mut assigned: Vec<_> = groups
            .groups
            .iter()
            .flat_map(|group| group.tabs.iter().copied())
            .collect();
        assigned.sort();
        assigned.dedup();
        assert_eq!(assigned.len(), tabs.len());
    }

    #[test]
    fn closing_last_pane_tab_keeps_empty_pane_and_new_connection_takes_focus() {
        let mut tabs = tabs(2);
        let mut groups = PaneGroups::default();
        let mut active = 0;
        groups.sync(&tabs, &mut active, TerminalLayout::Columns);
        close_tabs(&mut tabs, &mut active, 0, CloseTabs::Current);
        groups.sync(&tabs, &mut active, TerminalLayout::Columns);
        assert_eq!(active, tabs.len());
        assert!(groups.groups[0].tabs.is_empty());
        assert_eq!(groups.focused, 0);
        tabs.push(App::make_tab(Profile::default()));
        active = tabs.len() - 1;
        groups.sync(&tabs, &mut active, TerminalLayout::Columns);
        assert_eq!(groups.groups[0].selected, Some(tabs[active].id));
        // A new connection can replace the startup tab before the next UI frame.
        let old = tabs[active].id;
        tabs.push(App::make_tab(Profile::default()));
        tabs.retain(|tab| tab.id != old);
        active = tabs.len() - 1;
        let new = tabs[active].id;
        groups.sync(&tabs, &mut active, TerminalLayout::Columns);
        assert_eq!(groups.groups[0].selected, Some(new));
        assert_eq!(tabs[active].id, new);
    }

    struct Harness {
        ctx: egui::Context,
        tabs: Vec<Tab>,
        groups: PaneGroups,
        active: usize,
    }

    impl Harness {
        fn new() -> Self {
            let mut harness = Self {
                ctx: egui::Context::default(),
                tabs: tabs(4),
                groups: PaneGroups::default(),
                active: 0,
            };
            harness.frame(vec![]);
            harness
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> PaneResponse {
            self.frame_allowed(events, true)
        }

        fn frame_allowed(&mut self, events: Vec<egui::Event>, allow_input: bool) -> PaneResponse {
            let mut result = PaneResponse::default();
            let mut output = self.ctx.run_ui(
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
                        result = terminal_panes(
                            ui,
                            &mut self.tabs,
                            &mut self.active,
                            &Config::default(),
                            TerminalLayout::Columns,
                            &mut self.groups,
                            allow_input,
                        );
                    });
                },
            );
            output.textures_delta.clear();
            result
        }
    }

    fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn pointer_routes_text_to_hovered_pane_without_clicking() {
        let mut harness = Harness::new();
        for (pos, expected) in [(egui::pos2(600.0, 200.0), 1), (egui::pos2(100.0, 200.0), 0)] {
            for tab in &mut harness.tabs {
                tab.test_sent.clear();
            }
            harness.frame(vec![
                egui::Event::PointerMoved(pos),
                egui::Event::Text("*".into()),
            ]);
            assert_eq!(harness.active, expected);
            for (index, tab) in harness.tabs.iter().enumerate() {
                let inputs: Vec<_> = tab
                    .test_sent
                    .iter()
                    .filter_map(|cmd| match cmd {
                        ssh::Command::Input(bytes) => Some(bytes.as_slice()),
                        _ => None,
                    })
                    .collect();
                assert_eq!(
                    inputs,
                    if index == expected {
                        vec![b"*".as_slice()]
                    } else {
                        vec![]
                    }
                );
            }
        }
        harness.frame_allowed(
            vec![egui::Event::PointerMoved(egui::pos2(600.0, 200.0))],
            false,
        );
        assert_eq!(harness.active, 0);
        harness.frame(vec![
            egui::Event::PointerMoved(egui::pos2(100.0, 200.0)),
            button(egui::pos2(100.0, 200.0), true),
        ]);
        harness.frame(vec![egui::Event::PointerMoved(egui::pos2(600.0, 200.0))]);
        assert_eq!(
            harness.active, 0,
            "dragging a selection must not switch sessions"
        );
    }

    #[test]
    fn pane_tab_click_changes_only_that_pane_and_routes_input_once() {
        let mut harness = Harness::new();
        let pos = egui::pos2(600.0, 24.0);
        harness.frame(vec![egui::Event::PointerMoved(pos)]);
        harness.frame(vec![button(pos, true)]);
        harness.frame(vec![
            button(pos, false),
            egui::Event::Text("right-pane".into()),
        ]);
        assert_eq!(harness.active, 3);
        assert_eq!(harness.groups.groups[0].selected, Some(harness.tabs[0].id));
        for (index, tab) in harness.tabs.iter().enumerate() {
            let input: Vec<_> = tab
                .test_sent
                .iter()
                .filter_map(|command| match command {
                    ssh::Command::Input(bytes) => Some(bytes.as_slice()),
                    _ => None,
                })
                .flatten()
                .copied()
                .collect();
            assert_eq!(
                input,
                if index == 3 {
                    b"right-pane".to_vec()
                } else {
                    vec![]
                }
            );
        }
    }

    #[test]
    fn pane_tab_drag_moves_without_closing_or_reconnecting() {
        let mut harness = Harness::new();
        let id = harness.tabs[0].id;
        let start = egui::pos2(70.0, 24.0);
        let end = egui::pos2(600.0, 150.0);
        harness.frame(vec![egui::Event::PointerMoved(start)]);
        harness.frame(vec![button(start, true)]);
        harness.frame(vec![egui::Event::PointerMoved(end)]);
        harness.frame(vec![]);
        harness.frame(vec![button(end, false)]);
        assert!(!harness.groups.groups[0].tabs.contains(&id));
        assert!(harness.groups.groups[1].tabs.contains(&id));
        assert_eq!(harness.groups.groups[1].selected, Some(id));
        assert_eq!(harness.tabs.len(), 4);
        assert!(harness.tabs.iter().all(|tab| tab.connected));
    }
}
