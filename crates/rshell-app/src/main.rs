#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod composer_icons;
mod terminal_appearance;
mod terminal_view;
mod ui_appearance;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use eframe::egui;
use rshell_core::config::Config;
use std::{fs, path::PathBuf};

fn main() {
    if let Err(error) = run() {
        eprintln!("RShell: {error:#}");
        #[cfg(windows)]
        if !std::env::args().any(|arg| arg == "--smoke-test" || arg == "--agent-mcp") {
            show_error(&format!("RShell 启动失败\n\n{error:#}"));
        }
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "--apply-update")
    {
        return rshell_core::update::apply_update(&std::env::args_os().skip(2).collect::<Vec<_>>());
    }
    if std::env::args().any(|arg| arg == "--agent-mcp") {
        return rshell_core::claude_agent::serve_stdio();
    }
    let dir = match std::env::var_os("RSHELL_CONFIG_DIR") {
        Some(path) => PathBuf::from(path),
        None => ProjectDirs::from("com", "RShell", "RShell")
            .context("找不到用户配置目录")?
            .config_dir()
            .to_path_buf(),
    };
    fs::create_dir_all(&dir)?;
    let instance = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("instance.lock"))?;
    instance
        .try_lock()
        .context("RShell 已在运行，或无法锁定配置目录")?;
    let path = dir.join("config.json");
    let config = Config::load(&path)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()?;
    let demo = std::env::args().any(|arg| arg == "--demo");
    let smoke = std::env::args().any(|arg| arg == "--smoke-test");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RShell · Rust SSH Client")
            .with_inner_size([1180.0, 780.0])
            .with_min_inner_size([800.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native(
        "RShell",
        options,
        Box::new(move |cc| {
            install_fonts(&cc.egui_ctx, config.ui_font);
            Ok(Box::new(app::App::new(
                cc, config, path, runtime, demo, smoke,
            )?))
        }),
    )
    .map_err(|e| anyhow::anyhow!("原生窗口或图形后端初始化失败：{e}"))?;
    drop(instance);
    Ok(())
}

fn install_fonts(ctx: &egui::Context, ui_font: rshell_core::config::UiFont) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "geist".into(),
        egui::FontData::from_static(include_bytes!("../assets/fonts/Geist-Variable.ttf")).into(),
    );
    fonts.font_data.insert(
        "geist_mono".into(),
        egui::FontData::from_static(include_bytes!("../assets/fonts/GeistMono-Variable.ttf"))
            .into(),
    );
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "geist".into());
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .insert(0, "geist_mono".into());
    let windows = std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:\\Windows"));
    for (name, files) in [
        ("fira_code", &["FiraCode-Regular.ttf"][..]),
        ("jetbrains_mono", &["JetBrainsMono-Regular.ttf"][..]),
        (
            "cascadia_code",
            &["CascadiaCode.ttf", "CascadiaMono.ttf"][..],
        ),
        ("consolas", &["consola.ttf"][..]),
    ] {
        if let Some(bytes) = files
            .iter()
            .find_map(|file| fs::read(windows.join("Fonts").join(file)).ok())
        {
            fonts
                .font_data
                .insert(name.into(), egui::FontData::from_owned(bytes).into());
            fonts.families.insert(
                egui::FontFamily::Name(name.into()),
                vec![name.into(), "geist_mono".into()],
            );
        }
    }
    for (_, _, name) in terminal_appearance::FONTS {
        fonts
            .families
            .entry(egui::FontFamily::Name((*name).into()))
            .or_insert_with(|| vec!["geist_mono".into()]);
    }
    for filename in ["msyh.ttc", "simhei.ttf", "simsun.ttc"] {
        if let Ok(bytes) = fs::read(windows.join("Fonts").join(filename)) {
            fonts
                .font_data
                .insert("cjk".into(), egui::FontData::from_owned(bytes).into());
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(family).or_default().push("cjk".into());
            }
            if ui_font == rshell_core::config::UiFont::System {
                fonts
                    .families
                    .entry(egui::FontFamily::Proportional)
                    .or_default()
                    .insert(0, "cjk".into());
            }
            for (_, _, name) in terminal_appearance::FONTS {
                fonts
                    .families
                    .entry(egui::FontFamily::Name((*name).into()))
                    .or_default()
                    .push("cjk".into());
            }
            break;
        }
    }
    ctx.set_fonts(fonts);
}

#[cfg(windows)]
fn show_error(message: &str) {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(
            hwnd: *mut std::ffi::c_void,
            text: *const u16,
            caption: *const u16,
            kind: u32,
        ) -> i32;
    }
    let text: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    let title: Vec<u16> = "RShell".encode_utf16().chain(Some(0)).collect();
    // Both buffers are NUL terminated and live for the synchronous Windows call.
    unsafe {
        MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), 0x10);
    }
}
