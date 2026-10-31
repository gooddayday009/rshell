use eframe::egui::{self, Color32};
use rshell_core::config::{TerminalFont, TerminalTheme};

pub const FONTS: &[(TerminalFont, &str, &str)] = &[
    (TerminalFont::GeistMono, "Geist Mono", "geist_mono"),
    (TerminalFont::FiraCode, "Fira Code", "fira_code"),
    (
        TerminalFont::JetBrainsMono,
        "JetBrains Mono",
        "jetbrains_mono",
    ),
    (TerminalFont::CascadiaCode, "Cascadia Code", "cascadia_code"),
    (TerminalFont::Consolas, "Consolas", "consolas"),
];

pub const THEMES: &[(TerminalTheme, &str)] = &[
    (TerminalTheme::Xterm, "XTerm"),
    (TerminalTheme::OneDark, "One Dark"),
    (TerminalTheme::VsDark, "VS Dark+"),
    (TerminalTheme::VsLight, "VS Light+"),
    (TerminalTheme::Nord, "Nord"),
    (TerminalTheme::Okaidia, "Okaidia"),
    (TerminalTheme::Material, "Material"),
    (TerminalTheme::DuotoneLight, "Duotone Light"),
    (TerminalTheme::DuotoneDark, "Duotone Dark"),
    (TerminalTheme::Xcode, "Xcode"),
    (TerminalTheme::XcodeDark, "Xcode Dark"),
    (TerminalTheme::IdeaLight, "IDEA Light"),
    (TerminalTheme::IdeaDark, "IDEA Darcula"),
];

pub fn font_id(font: TerminalFont, size: f32) -> egui::FontId {
    if font == TerminalFont::GeistMono {
        return egui::FontId::monospace(size);
    }
    let name = FONTS.iter().find(|(value, _, _)| *value == font).unwrap().2;
    egui::FontId::new(size, egui::FontFamily::Name(name.into()))
}

pub fn font_available(font: TerminalFont) -> bool {
    let files: &[&str] = match font {
        TerminalFont::GeistMono => return true,
        TerminalFont::FiraCode => &["FiraCode-Regular.ttf"],
        TerminalFont::JetBrainsMono => &["JetBrainsMono-Regular.ttf"],
        TerminalFont::CascadiaCode => &["CascadiaCode.ttf", "CascadiaMono.ttf"],
        TerminalFont::Consolas => &["consola.ttf"],
    };
    let windows = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
    files.iter().any(|file| {
        std::path::Path::new(&windows)
            .join("Fonts")
            .join(file)
            .is_file()
    })
}

pub fn colors(theme: TerminalTheme) -> ([u8; 3], [u8; 3], [[u8; 3]; 16]) {
    // Terminal colors are adapted from the corresponding editor presets. ANSI colors
    // are terminal-specific; explicit RGB output is never recolored.
    let (background, foreground, accents) = match theme {
        TerminalTheme::Xterm => (
            [0, 0, 0],
            [229, 229, 229],
            [205, 0, 0, 0, 205, 0, 0, 0, 238],
        ),
        TerminalTheme::OneDark => (
            [40, 44, 52],
            [171, 178, 191],
            [224, 108, 117, 152, 195, 121, 97, 175, 239],
        ),
        TerminalTheme::VsDark => (
            [30, 30, 30],
            [212, 212, 212],
            [244, 112, 103, 181, 206, 168, 86, 156, 214],
        ),
        TerminalTheme::VsLight => (
            [255, 255, 255],
            [36, 36, 36],
            [163, 21, 21, 9, 134, 88, 0, 0, 255],
        ),
        TerminalTheme::Nord => (
            [46, 52, 64],
            [216, 222, 233],
            [191, 97, 106, 163, 190, 140, 94, 129, 172],
        ),
        TerminalTheme::Okaidia => (
            [39, 40, 34],
            [248, 248, 242],
            [249, 38, 114, 166, 226, 46, 102, 217, 239],
        ),
        TerminalTheme::Material => (
            [46, 50, 53],
            [189, 189, 189],
            [255, 83, 112, 195, 232, 141, 130, 170, 255],
        ),
        TerminalTheme::DuotoneLight => (
            [250, 248, 245],
            [55, 47, 58],
            [178, 40, 86, 48, 133, 93, 48, 93, 167],
        ),
        TerminalTheme::DuotoneDark => (
            [45, 38, 52],
            [210, 197, 214],
            [255, 99, 138, 167, 215, 115, 128, 170, 255],
        ),
        TerminalTheme::Xcode => (
            [255, 255, 255],
            [38, 38, 38],
            [196, 26, 22, 0, 128, 0, 0, 0, 151],
        ),
        TerminalTheme::XcodeDark => (
            [30, 30, 30],
            [223, 223, 223],
            [255, 105, 97, 148, 224, 136, 76, 143, 255],
        ),
        TerminalTheme::IdeaLight => (
            [255, 255, 255],
            [40, 40, 40],
            [193, 42, 50, 98, 150, 85, 0, 0, 193],
        ),
        TerminalTheme::IdeaDark => (
            [43, 43, 43],
            [169, 183, 198],
            [204, 120, 50, 106, 135, 89, 104, 151, 187],
        ),
    };
    if theme == TerminalTheme::Xterm {
        return (
            background,
            foreground,
            std::array::from_fn(rshell_terminal::xterm_color),
        );
    }
    let red = [accents[0], accents[1], accents[2]];
    let green = [accents[3], accents[4], accents[5]];
    let blue = [accents[6], accents[7], accents[8]];
    let yellow = accent_mix(red, green, 0.5);
    let cyan = accent_mix(green, blue, 0.5);
    let magenta = accent_mix(red, blue, 0.5);
    let dim = accent_mix(
        background,
        foreground,
        if background[0] > 180 { 0.8 } else { 0.4 },
    );
    let bright = accent_mix(background, foreground, 0.8);
    (
        background,
        foreground,
        [
            dim, red, green, yellow, blue, magenta, cyan, foreground, dim, red, green, yellow,
            blue, magenta, cyan, bright,
        ],
    )
}

fn accent_mix(a: [u8; 3], b: [u8; 3], factor: f32) -> [u8; 3] {
    std::array::from_fn(|i| (a[i] as f32 * (1.0 - factor) + b[i] as f32 * factor) as u8)
}

pub fn themed_color(rgb: [u8; 3], index: Option<usize>, theme: TerminalTheme) -> Color32 {
    let (bg, fg, ansi) = colors(theme);
    let target = match (theme, index) {
        (TerminalTheme::Xterm, _) | (_, None) => rgb,
        (_, Some(value @ 0..=15)) => ansi[value],
        (_, Some(16..=255)) => rgb,
        (_, Some(value)) if value == rshell_terminal::background_color_index() => bg,
        (_, Some(_)) => fg,
    };
    Color32::from_rgb(target[0], target[1], target[2])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_palette_and_truecolor_behavior() {
        assert_eq!(
            colors(TerminalTheme::Xterm).2[1],
            rshell_terminal::xterm_color(1)
        );
        assert_eq!(
            themed_color([40, 44, 52], None, TerminalTheme::OneDark),
            Color32::from_rgb(40, 44, 52)
        );
        assert_eq!(
            themed_color([12, 34, 56], None, TerminalTheme::OneDark),
            Color32::from_rgb(12, 34, 56)
        );
        assert_eq!(
            themed_color(
                [0, 0, 0],
                Some(rshell_terminal::background_color_index()),
                TerminalTheme::VsLight
            ),
            Color32::WHITE
        );
    }
}
