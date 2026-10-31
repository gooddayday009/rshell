use eframe::egui::{self, Color32};
use rshell_core::config::{Config, UiPalette};

pub const PALETTES: &[(UiPalette, &str, [u8; 3])] = &[
    (UiPalette::Rshell, "RShell 默认", [44, 99, 183]),
    (UiPalette::Pearl, "洁白", [255, 255, 255]),
    (UiPalette::Mist, "雾灰", [228, 234, 242]),
    (UiPalette::Graphite, "钛灰", [216, 220, 228]),
    (UiPalette::Cobalt, "钴蓝", [216, 230, 247]),
    (UiPalette::Sage, "青绿", [219, 233, 226]),
    (UiPalette::Amber, "暖黄", [244, 228, 184]),
    (UiPalette::Blush, "樱粉", [244, 217, 230]),
    (UiPalette::VsCode, "VS Code", [0, 122, 204]),
    (UiPalette::Idea, "IDEA", [75, 110, 175]),
    (UiPalette::Xcode, "Xcode", [10, 132, 255]),
    (UiPalette::JetBrains, "JetBrains", [123, 97, 255]),
    (UiPalette::Cursor, "Cursor", [107, 164, 255]),
    (UiPalette::Claude, "Claude Code", [196, 122, 80]),
    (UiPalette::Custom, "自定义", [23, 23, 23]),
];

#[derive(Clone, Copy)]
pub struct Colors {
    pub background: [u8; 3],
    pub foreground: [u8; 3],
    pub primary: [u8; 3],
    pub border: [u8; 3],
    pub sidebar: [u8; 3],
}

pub fn colors(config: &Config, dark: bool) -> Colors {
    use UiPalette::*;
    let (background, foreground, primary, border, sidebar) = match (config.ui_palette, dark) {
        (Rshell, false) => (
            [237, 243, 250],
            [43, 60, 79],
            [44, 99, 183],
            [193, 210, 229],
            [232, 240, 250],
        ),
        (Rshell, true) => (
            [29, 37, 50],
            [214, 220, 230],
            [55, 164, 232],
            [54, 70, 90],
            [23, 29, 40],
        ),
        (Pearl, false) => (
            [255, 255, 255],
            [10, 10, 10],
            [23, 23, 23],
            [229, 229, 229],
            [250, 250, 250],
        ),
        (Pearl, true) => (
            [19, 20, 22],
            [215, 215, 219],
            [208, 208, 214],
            [110, 110, 114],
            [25, 25, 28],
        ),
        (Mist, false) => (
            [250, 251, 253],
            [40, 44, 52],
            [46, 95, 166],
            [208, 215, 224],
            [239, 242, 246],
        ),
        (Mist, true) => (
            [31, 33, 37],
            [202, 206, 214],
            [157, 177, 205],
            [72, 76, 84],
            [34, 37, 42],
        ),
        (Graphite, false) => (
            [249, 250, 252],
            [42, 45, 51],
            [72, 80, 93],
            [210, 215, 224],
            [235, 238, 243],
        ),
        (Graphite, true) => (
            [30, 32, 36],
            [198, 201, 207],
            [174, 181, 192],
            [72, 76, 84],
            [33, 35, 40],
        ),
        (Cobalt, false) => (
            [247, 250, 253],
            [32, 43, 58],
            [43, 99, 183],
            [200, 212, 229],
            [232, 239, 248],
        ),
        (Cobalt, true) => (
            [25, 31, 42],
            [198, 207, 219],
            [137, 173, 223],
            [60, 72, 90],
            [28, 35, 48],
        ),
        (Sage, false) => (
            [248, 250, 248],
            [36, 48, 43],
            [58, 123, 106],
            [203, 217, 211],
            [235, 242, 238],
        ),
        (Sage, true) => (
            [27, 35, 33],
            [198, 208, 204],
            [138, 184, 169],
            [65, 78, 72],
            [30, 39, 36],
        ),
        (Amber, false) => (
            [255, 252, 242],
            [56, 48, 32],
            [149, 95, 28],
            [224, 207, 168],
            [247, 238, 213],
        ),
        (Amber, true) => (
            [37, 34, 27],
            [217, 208, 188],
            [213, 169, 75],
            [82, 74, 55],
            [41, 37, 29],
        ),
        (Blush, false) => (
            [255, 248, 251],
            [61, 48, 56],
            [184, 86, 123],
            [226, 202, 214],
            [248, 234, 241],
        ),
        (Blush, true) => (
            [42, 35, 40],
            [221, 205, 214],
            [226, 160, 184],
            [82, 68, 77],
            [47, 38, 45],
        ),
        (VsCode, false) => (
            [255, 255, 255],
            [51, 51, 51],
            [0, 122, 204],
            [224, 224, 224],
            [243, 243, 243],
        ),
        (VsCode, true) => (
            [30, 30, 30],
            [204, 204, 204],
            [0, 122, 204],
            [60, 60, 60],
            [37, 37, 38],
        ),
        (Idea, false) => (
            [250, 250, 250],
            [43, 43, 43],
            [75, 110, 175],
            [213, 215, 219],
            [235, 236, 240],
        ),
        (Idea, true) => (
            [43, 43, 43],
            [187, 187, 187],
            [106, 135, 197],
            [85, 86, 87],
            [49, 51, 53],
        ),
        (Xcode, false) => (
            [250, 252, 255],
            [31, 41, 55],
            [10, 132, 255],
            [207, 216, 228],
            [242, 245, 250],
        ),
        (Xcode, true) => (
            [31, 35, 41],
            [215, 221, 231],
            [10, 132, 255],
            [68, 74, 86],
            [37, 41, 50],
        ),
        (JetBrains, false) => (
            [248, 249, 251],
            [31, 35, 40],
            [108, 93, 211],
            [211, 216, 224],
            [242, 243, 245],
        ),
        (JetBrains, true) => (
            [30, 31, 34],
            [223, 225, 229],
            [123, 97, 255],
            [77, 80, 86],
            [43, 45, 48],
        ),
        (Cursor, false) => (
            [251, 252, 254],
            [31, 36, 45],
            [59, 130, 246],
            [213, 218, 226],
            [244, 245, 247],
        ),
        (Cursor, true) => (
            [15, 17, 26],
            [213, 215, 222],
            [107, 164, 255],
            [58, 62, 74],
            [21, 24, 33],
        ),
        (Claude, false) => (
            [251, 247, 239],
            [51, 43, 35],
            [184, 107, 60],
            [221, 205, 187],
            [239, 231, 220],
        ),
        (Claude, true) => (
            [33, 31, 28],
            [216, 208, 196],
            [210, 138, 95],
            [82, 74, 66],
            [42, 39, 35],
        ),
        (Custom, _) => {
            let c = &config.custom_ui_colors;
            (c.background, c.foreground, c.primary, c.border, c.sidebar)
        }
    };
    Colors {
        background,
        foreground,
        primary,
        border,
        sidebar,
    }
}

pub fn color(rgb: [u8; 3]) -> Color32 {
    Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}

pub fn text_on(rgb: [u8; 3]) -> Color32 {
    let brightness = (rgb[0] as u32 * 299 + rgb[1] as u32 * 587 + rgb[2] as u32 * 114) / 1000;
    if brightness > 145 {
        Color32::BLACK
    } else {
        Color32::WHITE
    }
}

fn mix(a: [u8; 3], b: [u8; 3], weight: f32) -> [u8; 3] {
    std::array::from_fn(|i| (a[i] as f32 * (1.0 - weight) + b[i] as f32 * weight).round() as u8)
}

pub fn apply_palette(ctx: &egui::Context, config: &Config) {
    for (theme, dark) in [(egui::Theme::Light, false), (egui::Theme::Dark, true)] {
        let c = colors(config, dark);
        ctx.style_mut_of(theme, |style| {
            let v = &mut style.visuals;
            let bg = color(c.background);
            let fg = color(c.foreground);
            let border = egui::Stroke::new(1.0, color(c.border));
            let subtle = color(mix(c.background, c.foreground, 0.07));
            let hover = color(mix(c.background, c.primary, 0.15));
            v.panel_fill = bg;
            v.window_fill = bg;
            v.extreme_bg_color = color(mix(c.background, c.foreground, 0.025));
            v.faint_bg_color = color(c.sidebar);
            v.override_text_color = None;
            v.window_stroke = border;
            v.widgets.noninteractive.bg_stroke = border;
            v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, fg);
            v.widgets.inactive.bg_fill = subtle;
            v.widgets.inactive.weak_bg_fill = subtle;
            v.widgets.inactive.bg_stroke = border;
            v.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, fg);
            v.widgets.hovered.bg_fill = hover;
            v.widgets.hovered.weak_bg_fill = hover;
            v.widgets.hovered.bg_stroke = border;
            v.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, fg);
            v.widgets.active.bg_fill = color(mix(c.background, c.primary, 0.25));
            v.widgets.active.weak_bg_fill = v.widgets.active.bg_fill;
            v.widgets.active.bg_stroke = border;
            v.widgets.active.fg_stroke = egui::Stroke::new(1.0, fg);
            v.selection.bg_fill = color(mix(c.background, c.primary, 0.35));
            v.selection.stroke = egui::Stroke::new(1.0, fg);
        });
    }
}
