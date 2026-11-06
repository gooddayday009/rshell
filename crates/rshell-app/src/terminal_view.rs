use crate::app::Tab;
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use rshell_core::ssh::Command;
use rshell_terminal::{Flags, TermMode};

pub fn show(
    ui: &mut egui::Ui,
    tab: &mut Tab,
    config: &rshell_core::config::Config,
    allow_input: bool,
) -> Option<String> {
    crate::tunnel_view::status(ui, tab);
    if tab.profile.tunnel_only {
        return None;
    }
    // A newly spawned process can still be initializing. Do not queue invisible commands.
    let allow_input = allow_input && (!tab.local || tab.connected);
    let mut selection_for_ai = None;
    let font_size = config.font_size;
    if tab.show_search {
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), 20.0),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                ui.style_mut().override_font_id = Some(egui::FontId::proportional(11.0));
                ui.spacing_mut().interact_size.y = 20.0;
                if ui.small_button("关闭查找").clicked() {
                    tab.show_search = false;
                    tab.focus = true;
                }
                if tab.show_search {
                    let next = ui.small_button("下一个").clicked();
                    let previous = ui.small_button("上一个").clicked();
                    let input = ui.add(
                        egui::TextEdit::singleline(&mut tab.search)
                            .id(ui.scope_id().with(("search", tab.id)))
                            .font(egui::FontId::proportional(11.0))
                            .hint_text("查找终端历史")
                            .desired_width((ui.available_width() - 4.0).clamp(40.0, 180.0)),
                    );
                    if input.has_focus() {
                        tab.focus = false;
                    }
                    if input.changed() {
                        tab.search_position = None;
                    }
                    if previous || next {
                        tab.search_position = tab.terminal.lock().unwrap().find(
                            &tab.search,
                            tab.search_position,
                            previous,
                        );
                        if tab.search_position.is_none() {
                            ui.label("未找到");
                        }
                    }
                }
            },
        );
    }
    // Scale only this local or SSH terminal; never change egui's global zoom or saved defaults.
    if allow_input
        && (tab.local || tab.profile.protocol == rshell_core::config::Protocol::Ssh)
        && ui.rect_contains_pointer(ui.available_rect_before_wrap())
    {
        let steps: f32 = ui.input(|input| {
            input
                .events
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
                        return Some(
                            delta.y
                                * match unit {
                                    egui::MouseWheelUnit::Point => 1.0 / 50.0,
                                    egui::MouseWheelUnit::Line => 1.0,
                                    egui::MouseWheelUnit::Page => 3.0,
                                },
                        );
                    }
                    None
                })
                .sum()
        });
        if steps != 0.0 {
            tab.font_size_override =
                Some((tab.font_size_override.unwrap_or(font_size) + steps).clamp(10.0, 32.0));
            ui.input_mut(|input| input.smooth_scroll_delta = Vec2::ZERO);
        }
    }
    let font_size = tab.font_size_override.unwrap_or(font_size);
    let font = crate::terminal_appearance::font_id(config.terminal_font, font_size);
    let width = ui.fonts_mut(|f| f.glyph_width(&font, 'M')).max(1.0);
    let height = ui.fonts_mut(|f| f.row_height(&font)).max(1.0);
    let (rect, _) = ui.allocate_exact_size(
        ui.available_size().max(Vec2::new(40.0, 40.0)),
        Sense::hover(),
    );
    // Keep focus identity stable when the optional search row appears/disappears.
    let response = ui.interact(
        rect,
        ui.scope_id().with(("terminal_input", tab.id)),
        Sense::click_and_drag(),
    );
    let inner = Rect::from_min_max(rect.min + Vec2::splat(8.0), rect.max - Vec2::new(24.0, 8.0));
    let cols = (inner.width() / width).floor().clamp(2.0, 500.0) as u16;
    let rows = (inner.height() / height).floor().clamp(1.0, 200.0) as u16;
    if (cols, rows) != tab.size {
        let resized = if tab.connected {
            tab.send(Command::Resize { cols, rows });
            true
        } else if let Ok(mut terminal) = tab.terminal.try_lock() {
            terminal.resize(cols, rows);
            true
        } else {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(16));
            false
        };
        if resized {
            tab.size = (cols, rows);
        }
    }
    let pointer_inside = response.hovered();
    let pointer_entered = pointer_inside && !tab.pointer_in_terminal;
    tab.pointer_in_terminal = pointer_inside;
    if allow_input
        && tab.pending_paste.is_none()
        && !response.context_menu_opened()
        && (response.clicked() || tab.focus || pointer_entered)
    {
        response.request_focus();
        // egui only accepts the focus filter after one focused frame. Keep the
        // request alive until then so Tab/Escape cannot escape on the first key.
        tab.focus = !ui.memory(|memory| memory.had_focus_last_frame(response.id));
        ui.ctx().request_repaint();
    }
    if response.has_focus() {
        ui.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                response.id,
                egui::EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                },
            )
        });
    }
    // Output parsing must never stall an otherwise idle UI frame.
    if let Ok(mut terminal) = tab.terminal.try_lock() {
        if tab.pending_bottom {
            terminal.bottom();
            tab.pending_bottom = false;
        }
        tab.render_snapshot = Some(terminal.cached_snapshot());
    } else {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(16));
    }
    let mut snapshot = tab.render_snapshot.clone()?;
    let shift = ui.input(|i| i.modifiers.shift);
    let mouse_reporting = snapshot.mode.intersects(TermMode::MOUSE_MODE) && !shift && tab.connected;
    if allow_input && !mouse_reporting {
        response.context_menu(|ui| {
            let selected = tab.terminal.lock().unwrap().selection_text();
            if ui
                .add_enabled(!selected.is_empty(), egui::Button::new("复制"))
                .clicked()
            {
                ui.ctx().copy_text(selected.clone());
                ui.close();
            }
            if ui.button("粘贴").clicked() {
                tab.focus = true;
                response.request_focus();
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::RequestPaste);
                ui.close();
            }
            ui.separator();
            if ui
                .add_enabled(
                    !selected.is_empty(),
                    egui::Button::new("添加选中内容到 AI 对话"),
                )
                .clicked()
            {
                selection_for_ai = Some(selected);
                ui.close();
            }
        });
    }
    let scrollbar = Rect::from_min_max(
        egui::pos2(rect.right() - 18.0, rect.top() + 8.0),
        egui::pos2(rect.right() - 4.0, rect.bottom() - 8.0),
    );
    let scrollbar_response = ui.interact(
        scrollbar,
        ui.scope_id().with(("terminal_scrollbar", tab.id)),
        Sense::click_and_drag(),
    );
    if (scrollbar_response.clicked() || scrollbar_response.dragged())
        && snapshot.history_size > 0
        && let Some(pos) = scrollbar_response.interact_pointer_pos()
    {
        let thumb_height = (scrollbar.height() * snapshot.rows as f32
            / (snapshot.rows + snapshot.history_size) as f32)
            .clamp(20.0, scrollbar.height());
        let travel = (scrollbar.height() - thumb_height).max(1.0);
        let fraction = ((scrollbar.bottom() - pos.y - thumb_height * 0.5) / travel).clamp(0.0, 1.0);
        let scroll_offset = (fraction * snapshot.history_size as f32).round() as i32;
        tab.scroll_remainder = 0.0;
        tab.terminal
            .lock()
            .unwrap()
            .scroll(scroll_offset - snapshot.scroll_offset as i32);
        snapshot = tab.terminal.lock().unwrap().cached_snapshot();
    }
    let point = |pos: Pos2| -> (usize, usize) {
        (
            ((pos.y - inner.top()) / height)
                .floor()
                .clamp(0.0, rows.saturating_sub(1) as f32) as usize,
            ((pos.x - inner.left()) / width)
                .floor()
                .clamp(0.0, cols.saturating_sub(1) as f32) as usize,
        )
    };
    if allow_input
        && response.drag_started_by(egui::PointerButton::Primary)
        && !mouse_reporting
        && let Some(pos) = ui.input(|i| i.pointer.press_origin())
        && inner.contains(pos)
    {
        let (row, col) = point(pos);
        tab.terminal.lock().unwrap().start_selection(
            row,
            col,
            selection_side(pos.x, inner.left(), width),
        );
    }
    if allow_input
        && (response.dragged_by(egui::PointerButton::Primary)
            || response.drag_stopped_by(egui::PointerButton::Primary))
        && !mouse_reporting
        && let Some(pos) = response.interact_pointer_pos()
    {
        let (row, col) = point(pos);
        tab.terminal.lock().unwrap().extend_selection(
            row,
            col,
            selection_side(pos.x, inner.left(), width),
        );
        snapshot = tab.terminal.lock().unwrap().cached_snapshot();
        ui.ctx().request_repaint();
    }
    if allow_input
        && response.drag_stopped_by(egui::PointerButton::Primary)
        && !mouse_reporting
        && config.copy_on_select
    {
        let text = tab.terminal.lock().unwrap().selection_text();
        if !text.is_empty() {
            ui.ctx().copy_text(text);
        }
    }
    let painter = ui.painter_at(rect);
    let theme = config.terminal_theme;
    let (background, _, _) = crate::terminal_appearance::colors(theme);
    painter.rect_filled(
        rect,
        0.0,
        Color32::from_rgb(background[0], background[1], background[2]),
    );
    let position =
        |row: usize, col: usize| inner.min + Vec2::new(col as f32 * width, row as f32 * height);
    let mut highlights = std::collections::HashSet::new();
    if tab.show_search && !tab.search.is_empty() {
        for row in 0..snapshot.rows {
            let cells: Vec<_> = snapshot
                .cells
                .iter()
                .filter(|c| c.row == row && !c.flags.contains(Flags::WIDE_CHAR_SPACER))
                .collect();
            let mut text = String::new();
            let mut spans = Vec::new();
            for cell in cells {
                let start = text.len();
                text.push_str(&cell.text);
                spans.push((start, text.len(), cell.col));
            }
            for (start, found) in text.match_indices(&tab.search) {
                for (a, b, col) in &spans {
                    if *a < start + found.len() && *b > start {
                        highlights.insert((row, *col));
                    }
                }
            }
        }
    }
    // Paint all backgrounds first so a wide glyph isn't covered by its spacer's background.
    let mut background_run: Option<(Rect, Color32)> = None;
    for cell in &snapshot.cells {
        if cell.row >= snapshot.rows {
            continue;
        }
        let pos = position(cell.row, cell.col);
        let cell_rect = Rect::from_min_size(pos, Vec2::new(width, height));
        let bg = if cell.selected {
            Color32::from_rgb(52, 78, 110)
        } else if highlights.contains(&(cell.row, cell.col)) {
            Color32::from_rgb(117, 88, 22)
        } else {
            crate::terminal_appearance::themed_color(cell.bg, cell.bg_index, theme)
        };
        if let Some((run, color)) = &mut background_run
            && *color == bg
            && run.top() == cell_rect.top()
            && (run.right() - cell_rect.left()).abs() < 0.1
        {
            run.max.x = cell_rect.max.x;
        } else {
            if let Some((run, color)) = background_run.take() {
                painter.rect_filled(run, 0.0, color);
            }
            if bg != Color32::from_rgb(background[0], background[1], background[2]) {
                background_run = Some((cell_rect, bg));
            }
        }
    }
    if let Some((run, color)) = background_run {
        painter.rect_filled(run, 0.0, color);
    }
    let mut cell_index = 0;
    while cell_index < snapshot.cells.len() {
        let cell = &snapshot.cells[cell_index];
        cell_index += 1;
        if cell.row >= snapshot.rows {
            continue;
        }
        let pos = position(cell.row, cell.col);
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER | Flags::HIDDEN)
        {
            continue;
        }
        let mut fg = crate::terminal_appearance::themed_color(cell.fg, cell.fg_index, theme);
        if theme != rshell_core::config::TerminalTheme::Xterm
            && cell.flags.contains(Flags::DIM)
            && cell.fg_index.is_some()
        {
            fg = Color32::from_rgb(
                (fg.r() as f32 * 0.65) as u8,
                (fg.g() as f32 * 0.65) as u8,
                (fg.b() as f32 * 0.65) as u8,
            );
        }
        let mut text = cell.text.clone();
        let mut end = cell_index;
        if cell.text.len() == 1 && cell.text.is_ascii() {
            while let Some(next) = snapshot.cells.get(end) {
                if next.row != cell.row
                    || next.col != cell.col + end - cell_index + 1
                    || next.flags != cell.flags
                    || next.fg != cell.fg
                    || next.fg_index != cell.fg_index
                    || next.text.len() != 1
                    || !next.text.is_ascii()
                {
                    break;
                }
                text.push_str(&next.text);
                end += 1;
            }
        }
        let span = end - cell_index + 1;
        cell_index = end;
        if !text.chars().all(|c| c == ' ') {
            let mut galley = painter.layout_no_wrap(text, font.clone(), fg);
            if span > 1 {
                align_ascii_run(&mut galley, width);
            }
            painter.galley(pos, galley.clone(), fg);
            if cell.flags.contains(Flags::BOLD) {
                painter.galley(pos + Vec2::new(0.4, 0.0), galley, fg);
            }
        }
        if cell.flags.intersects(Flags::ALL_UNDERLINES) {
            painter.line_segment(
                [
                    pos + Vec2::new(0.0, height - 2.0),
                    pos + Vec2::new(width * span as f32, height - 2.0),
                ],
                Stroke::new(1.0, fg),
            );
        }
        if cell.flags.contains(Flags::STRIKEOUT) {
            painter.line_segment(
                [
                    pos + Vec2::new(0.0, height * 0.5),
                    pos + Vec2::new(width * span as f32, height * 0.5),
                ],
                Stroke::new(1.0, fg),
            );
        }
    }
    let cursor_pos = snapshot
        .cursor
        .map(|(row, col)| position(row, col))
        .unwrap_or(inner.min);
    let cursor_rect = Rect::from_min_size(cursor_pos, Vec2::new(width, height));
    if snapshot.cursor.is_some() {
        painter.rect_filled(cursor_rect, 0.0, Color32::from_rgb(0, 255, 0));
        if let Some(cell) = snapshot
            .cells
            .iter()
            .find(|cell| Some((cell.row, cell.col)) == snapshot.cursor)
        {
            painter.text(
                cursor_pos,
                egui::Align2::LEFT_TOP,
                &cell.text,
                font.clone(),
                Color32::BLACK,
            );
        }
    }
    if !tab.preedit.is_empty() {
        let galley = painter.layout_no_wrap(tab.preedit.clone(), font.clone(), Color32::WHITE);
        painter.rect_filled(
            Rect::from_min_size(cursor_pos, galley.size()),
            0.0,
            Color32::from_rgb(50, 65, 85),
        );
        painter.galley(cursor_pos, galley, Color32::WHITE);
    }
    painter.rect_filled(scrollbar, 2.0, Color32::from_rgb(35, 43, 55));
    let thumb_height = (scrollbar.height() * snapshot.rows as f32
        / (snapshot.rows + snapshot.history_size) as f32)
        .clamp(20.0, scrollbar.height());
    let travel = scrollbar.height() - thumb_height;
    let fraction = if snapshot.history_size == 0 {
        0.0
    } else {
        snapshot.scroll_offset as f32 / snapshot.history_size as f32
    };
    let thumb = Rect::from_min_size(
        egui::pos2(
            scrollbar.left(),
            scrollbar.bottom() - thumb_height - travel * fraction,
        ),
        Vec2::new(scrollbar.width(), thumb_height),
    );
    painter.rect_filled(
        thumb,
        2.0,
        if scrollbar_response.hovered() || scrollbar_response.dragged() {
            Color32::from_rgb(164, 179, 199)
        } else {
            Color32::from_rgb(112, 129, 151)
        },
    );
    let has_input_focus = allow_input
        && response.has_focus()
        && !response.context_menu_opened()
        && tab.pending_paste.is_none();
    let focused = has_input_focus && ui.ctx().current_pass_index() == 0;
    if !has_input_focus {
        // Composition belongs to this focused terminal, never to a previous widget/session.
        tab.preedit.clear();
    }
    if focused {
        ui.output_mut(|output| {
            output.ime = Some(egui::output::IMEOutput {
                purpose: egui::IMEPurpose::Normal,
                rect: inner,
                cursor_rect,
                should_interrupt_composition: false,
            })
        });
        let events = ui.input(|input| input.events.clone());
        for (index, event) in events.iter().cloned().enumerate() {
            match event {
                egui::Event::Text(text) if tab.preedit.is_empty() => {
                    let alt = match index.checked_sub(1).and_then(|i| events.get(i)) {
                        Some(egui::Event::Key { modifiers, .. }) => modifiers.alt,
                        _ => ui.input(|i| i.modifiers.alt),
                    };
                    let mut data = if alt { vec![27] } else { vec![] };
                    data.extend(text.as_bytes());
                    tab.input(data);
                }
                egui::Event::Ime(egui::ImeEvent::Preedit { text, .. }) => tab.preedit = text,

                egui::Event::Ime(egui::ImeEvent::Commit(text)) => {
                    tab.preedit.clear();
                    tab.input(text.into_bytes());
                }
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } if tab.preedit.is_empty() => {
                    if crate::app::shortcut_matches(
                        &config.shortcuts.copy_selection,
                        key,
                        modifiers,
                    ) {
                        ui.ctx()
                            .copy_text(tab.terminal.lock().unwrap().selection_text());
                    } else if !modifiers.alt
                        && modifiers.shift
                        && ((modifiers.ctrl && key == egui::Key::V)
                            || (!modifiers.ctrl && key == egui::Key::Insert))
                    {
                        ui.ctx()
                            .send_viewport_cmd(egui::ViewportCommand::RequestPaste);
                    } else if modifiers.ctrl
                        && modifiers.shift
                        && !modifiers.alt
                        && key == egui::Key::A
                    {
                        tab.terminal.lock().unwrap().clear_screen();
                    } else if crate::app::shortcut_matches(
                        &config.shortcuts.search_terminal,
                        key,
                        modifiers,
                    ) {
                        tab.show_search = !tab.show_search;
                    } else if modifiers.shift
                        && !modifiers.ctrl
                        && !modifiers.alt
                        && key == egui::Key::Home
                    {
                        tab.terminal
                            .lock()
                            .unwrap()
                            .scroll(snapshot.history_size.min(i32::MAX as usize) as i32);
                    } else if modifiers.shift
                        && !modifiers.ctrl
                        && !modifiers.alt
                        && key == egui::Key::End
                    {
                        tab.terminal.lock().unwrap().bottom();
                    } else if modifiers.shift
                        && !modifiers.ctrl
                        && !modifiers.alt
                        && key == egui::Key::ArrowUp
                    {
                        tab.terminal.lock().unwrap().scroll(1);
                    } else if modifiers.shift
                        && !modifiers.ctrl
                        && !modifiers.alt
                        && key == egui::Key::ArrowDown
                    {
                        tab.terminal.lock().unwrap().scroll(-1);
                    } else if modifiers.shift
                        && !modifiers.ctrl
                        && !modifiers.alt
                        && key == egui::Key::PageUp
                    {
                        tab.terminal.lock().unwrap().scroll(rows as i32);
                    } else if modifiers.shift
                        && !modifiers.ctrl
                        && !modifiers.alt
                        && key == egui::Key::PageDown
                    {
                        tab.terminal.lock().unwrap().scroll(-(rows as i32));
                    } else if let Some(bytes) = rshell_terminal::key(
                        key.name(),
                        modifiers.ctrl,
                        modifiers.alt,
                        modifiers.shift,
                        snapshot.mode,
                    ) {
                        // Some backends emit both Key and Text for Alt+letter; others only Key.
                        // Prefer the following text (keyboard layout/case), without sending twice.
                        let text_follows = modifiers.alt
                            && !modifiers.ctrl
                            && bytes.len() == 2
                            && bytes[0] == 27
                            && bytes[1].is_ascii_graphic()
                            && matches!(events.get(index + 1), Some(egui::Event::Text(_)));
                        if !text_follows {
                            tab.input(bytes);
                        }
                    }
                }
                egui::Event::Copy => {
                    // eframe maps Ctrl+C to Copy; terminals must still send SIGINT.
                    if ui.input(|i| {
                        crate::app::shortcut_matches(
                            &config.shortcuts.copy_selection,
                            egui::Key::C,
                            i.modifiers,
                        )
                    }) {
                        ui.ctx()
                            .copy_text(tab.terminal.lock().unwrap().selection_text());
                    } else {
                        tab.input(vec![3]);
                    }
                }
                egui::Event::Cut => tab.input(vec![24]),
                egui::Event::Paste(text) => {
                    if text.len() > 64 * 1024 {
                        tab.status = "粘贴超过 64 KiB，请分批输入或使用文件传输".into();
                    } else {
                        let text = if config.trim_paste {
                            text.trim_end().to_owned()
                        } else {
                            text
                        };
                        if text.contains(['\n', '\r']) {
                            tab.pending_paste = Some(text);
                        } else if !text.is_empty() {
                            tab.input(rshell_terminal::paste(&text, snapshot.mode));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    if response.hovered() && allow_input && ui.ctx().current_pass_index() == 0 {
        let delta = ui.input(|i| i.smooth_scroll_delta.y);
        let lines = scroll_lines(&mut tab.scroll_remainder, delta, height);
        if lines != 0 {
            if mouse_reporting {
                if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                    let (row, col) = point(pos);
                    for _ in 0..lines.unsigned_abs().min(100) {
                        tab.input(mouse(
                            snapshot.mode,
                            if lines > 0 { 64 } else { 65 },
                            row,
                            col,
                            false,
                        ));
                    }
                }
            } else if snapshot.mode.contains(TermMode::ALT_SCREEN) && !shift {
                let name = if lines > 0 { "ArrowUp" } else { "ArrowDown" };
                if let Some(bytes) = rshell_terminal::key(name, false, false, false, snapshot.mode)
                {
                    for _ in 0..lines.unsigned_abs().min(100) {
                        tab.input(bytes.clone());
                    }
                }
            } else {
                tab.terminal.lock().unwrap().scroll(lines);
            }
        }
        if mouse_reporting {
            for event in ui.input(|i| i.events.clone()) {
                match event {
                    egui::Event::PointerButton {
                        pos,
                        button,
                        pressed,
                        ..
                    } if inner.contains(pos) => {
                        let code = match button {
                            egui::PointerButton::Primary => 0,
                            egui::PointerButton::Middle => 1,
                            egui::PointerButton::Secondary => 2,
                            _ => continue,
                        };
                        let (row, col) = point(pos);
                        tab.input(mouse(snapshot.mode, code, row, col, !pressed));
                    }
                    egui::Event::PointerMoved(pos)
                        if snapshot
                            .mode
                            .intersects(TermMode::MOUSE_MOTION | TermMode::MOUSE_DRAG)
                            && inner.contains(pos) =>
                    {
                        let pressed = ui.input(|i| i.pointer.primary_down());
                        if pressed || snapshot.mode.contains(TermMode::MOUSE_MOTION) {
                            let (row, col) = point(pos);
                            tab.input(mouse(
                                snapshot.mode,
                                if pressed { 32 } else { 35 },
                                row,
                                col,
                                false,
                            ));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    if let Some(text) = tab.pending_paste.clone() {
        let mut decision = None;
        egui::Modal::new(egui::Id::unique(("paste", tab.id))).show(ui.ctx(), |ui| {
            ui.heading("确认多行粘贴");
            ui.label("多行文本可能立即执行命令。请确认目标连接和内容。");
            ui.label(format!(
                "目标：{}@{}:{}",
                tab.profile.username, tab.profile.host, tab.profile.port
            ));
            egui::ScrollArea::vertical()
                .max_height(200.0)
                .show(ui, |ui| {
                    ui.monospace(&text);
                });
            ui.horizontal(|ui| {
                if ui.button("取消").clicked() {
                    decision = Some(false);
                }
                if ui.button("粘贴").clicked() {
                    decision = Some(true);
                }
            });
        });
        if let Some(accepted) = decision {
            tab.pending_paste = None;
            if accepted {
                tab.input(rshell_terminal::paste(&text, snapshot.mode));
            }
            tab.focus = true;
        }
    }
    selection_for_ai
}

// Keep fixed terminal cell positions even when font layout rounds advances or applies kerning.
// Only ASCII runs reach this helper; wide and combining characters retain individual layout.
fn align_ascii_run(galley: &mut std::sync::Arc<egui::Galley>, width: f32) {
    let galley = std::sync::Arc::make_mut(galley);
    let row = std::sync::Arc::make_mut(&mut galley.rows[0].row);
    for (i, glyph) in row.glyphs.iter_mut().enumerate() {
        let x = i as f32 * width;
        if !glyph.uv_rect.is_nothing() {
            let desired_left = x
                + (glyph.uv_rect.offset.x * galley.pixels_per_point).round()
                    / galley.pixels_per_point;
            let start = glyph.first_vertex as usize;
            let shift = desired_left - row.visuals.mesh.vertices[start].pos.x;
            for vertex in &mut row.visuals.mesh.vertices[start..start + 4] {
                vertex.pos.x += shift;
            }
        }
        glyph.pos.x = x;
    }
    row.size.x = row.glyphs.len() as f32 * width;
    row.visuals.mesh_bounds = Rect::NOTHING;
    for vertex in &row.visuals.mesh.vertices {
        row.visuals.mesh_bounds.extend_with(vertex.pos);
    }
    galley.mesh_bounds = row.visuals.mesh_bounds;
    galley.rect.max.x = row.size.x;
}

// egui already spreads wheel movement over frames. Keep sub-row movement
// instead of multiplying every animation frame into a fixed three-line jump.
fn scroll_lines(remainder: &mut f32, delta: f32, row_height: f32) -> i32 {
    if delta == 0.0 {
        return 0;
    }
    if remainder.signum() != delta.signum() {
        *remainder = 0.0;
    }
    *remainder += delta / row_height.max(1.0);
    let lines = remainder.trunc() as i32;
    *remainder -= lines as f32;
    lines
}

fn mouse(mode: TermMode, code: u8, row: usize, col: usize, released: bool) -> Vec<u8> {
    if mode.contains(TermMode::SGR_MOUSE) {
        format!(
            "\x1b[<{code};{};{}{}",
            col + 1,
            row + 1,
            if released { 'm' } else { 'M' }
        )
        .into_bytes()
    } else {
        vec![
            27,
            b'[',
            b'M',
            32 + if released { 3 } else { code },
            33 + col.min(222) as u8,
            33 + row.min(222) as u8,
        ]
    }
}

// Select the nearest cell boundary in either drag direction.
fn selection_side(x: f32, left: f32, width: f32) -> rshell_terminal::Side {
    if ((x - left) / width).rem_euclid(1.0) < 0.5 {
        rshell_terminal::Side::Left
    } else {
        rshell_terminal::Side::Right
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smooth_wheel_distance_is_independent_of_frame_count() {
        let mut remainder = 0.0;
        let small: i32 = (0..120)
            .map(|_| scroll_lines(&mut remainder, 1.0, 20.0))
            .sum();
        let mut single = 0.0;
        assert_eq!(small, scroll_lines(&mut single, 120.0, 20.0));
        assert_eq!(small, 6);
        assert_eq!(scroll_lines(&mut remainder, 5.0, 20.0), 0);
        assert_eq!(scroll_lines(&mut remainder, -20.0, 20.0), -1);
        assert_eq!(scroll_lines(&mut remainder, 0.0, 20.0), 0);
    }

    #[test]
    fn ascii_batch_preserves_fixed_grid_and_mesh_bounds() {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            for size in [11.0, 14.0, 19.0] {
                let font = egui::FontId::monospace(size);
                let width = ui.fonts_mut(|f| f.glyph_width(&font, 'M'));
                let mut galley =
                    ui.painter()
                        .layout_no_wrap("AV  iW0123".into(), font, Color32::WHITE);
                align_ascii_run(&mut galley, width);
                let row = &galley.rows[0];
                for (i, glyph) in row.glyphs.iter().enumerate() {
                    assert!((glyph.pos.x - i as f32 * width).abs() < 0.001);
                }
                for vertex in &row.visuals.mesh.vertices {
                    assert!(galley.mesh_bounds.contains(vertex.pos));
                }
                assert!((galley.rect.width() - 10.0 * width).abs() < 0.001);
            }
        });
        output.textures_delta.clear();
    }
}
