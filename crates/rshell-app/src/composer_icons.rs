//! Lucide icons used by dbx, rendered natively. See assets/lucide/LICENSE.
use eframe::egui;

pub enum Icon {
    Folder,
    File,
    Agent,
    Ask,
    Model,
    Help,
    Send,
}

#[cfg(test)]
pub fn find_text<'a>(shape: &'a egui::Shape, text: &str) -> Option<&'a egui::epaint::TextShape> {
    match shape {
        egui::Shape::Text(s) if s.galley.job.text == text => Some(s),
        egui::Shape::Vec(shapes) => shapes.iter().find_map(|s| find_text(s, text)),
        _ => None,
    }
}

pub fn image(icon: Icon, color: egui::Color32) -> egui::Image<'static> {
    let source = match icon {
        Icon::Folder => egui::include_image!("../assets/lucide/folder.svg"),
        Icon::File => egui::include_image!("../assets/lucide/file.svg"),
        Icon::Agent => egui::include_image!("../assets/lucide/bot.svg"),
        Icon::Ask => egui::include_image!("../assets/lucide/message-square-plus.svg"),
        Icon::Model => egui::include_image!("../assets/lucide/cpu.svg"),
        Icon::Help => egui::include_image!("../assets/lucide/circle-question-mark.svg"),
        Icon::Send => egui::include_image!("../assets/lucide/arrow-up.svg"),
    };
    egui::Image::new(source)
        .fit_to_exact_size(egui::vec2(14.0, 14.0))
        .tint(color)
}

pub fn send_button(ui: &mut egui::Ui, enabled: bool, accent: egui::Color32) -> egui::Response {
    ui.add_enabled_ui(enabled, |ui| {
        let (_, rect) = ui.allocate_space(egui::vec2(26.0, 26.0));
        let response = ui.interact(rect, egui::Id::unique("ai_send"), egui::Sense::click());
        let fill = if enabled {
            accent
        } else {
            accent.gamma_multiply(0.45)
        };
        ui.painter().rect_filled(rect, 13.0, fill);
        image(
            Icon::Send,
            if enabled {
                egui::Color32::WHITE
            } else {
                ui.visuals().weak_text_color()
            },
        )
        .paint_at(ui, rect.shrink(5.0));
        response
            .on_hover_text("发送 · Enter")
            .on_disabled_hover_text("输入内容后发送")
    })
    .inner
}
