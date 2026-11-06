use crate::app::Tab;
use eframe::egui::{self, RichText};
use rshell_core::{
    config::Profile,
    tunnel::{Kind, MAX_RULES, Rule},
};

pub fn settings(ui: &mut egui::Ui, profile: &mut Profile) {
    ui.label(RichText::new("SSH 隧道").size(22.0).strong());
    ui.label("认证成功后自动启动；任一规则启动失败将关闭本次连接。");
    ui.checkbox(&mut profile.tunnel_only, "仅建立隧道，不打开远程终端");
    ui.weak("修改规则后重新连接生效。断开或关闭所属标签会停止全部转发。");
    ui.add_space(10.0);
    let mut remove = None;
    for (index, rule) in profile.tunnels.iter_mut().enumerate() {
        ui.push_id(index, |ui| {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.checkbox(&mut rule.enabled, format!("规则 {}", index + 1));
                    egui::ComboBox::from_id_salt("kind").selected_text(rule.kind.label()).show_ui(ui, |ui| {
                        for kind in [Kind::Local, Kind::Remote, Kind::Dynamic] {
                            ui.selectable_value(&mut rule.kind, kind, kind.label());
                        }
                    });
                    if ui.small_button("删除").clicked() { remove = Some(index); }
                });
                ui.add_enabled_ui(rule.enabled, |ui| {
                    egui::Grid::new("fields").spacing([12.0, 8.0]).show(ui, |ui| {
                        ui.label(if rule.kind == Kind::Remote { "服务端监听" } else { "本机监听" });
                        endpoint(ui, &mut rule.bind_host, &mut rule.bind_port);
                        ui.end_row();
                        if rule.kind != Kind::Dynamic {
                            ui.label(if rule.kind == Kind::Remote { "本机访问目标" } else { "服务端访问目标" });
                            endpoint(ui, &mut rule.target_host, &mut rule.target_port);
                            ui.end_row();
                        }
                    });
                    match rule.kind {
                        Kind::Local => { ui.weak("目标地址从 SSH 服务端访问，例如内网数据库的主机和端口。"); }
                        Kind::Remote => { ui.weak("目标地址从本机访问；服务端须允许远程转发，监听范围由其策略决定。"); }
                        Kind::Dynamic => { ui.weak("无认证 SOCKS5，仅限回环监听；支持 TCP CONNECT、IPv4 / IPv6 和远端域名解析。"); }
                    }
                    if rule.bind_host.parse::<std::net::IpAddr>().is_ok_and(|ip| !ip.is_loopback()) {
                        ui.colored_label(ui.visuals().warn_fg_color, "非回环监听可能允许其他设备使用此转发，请检查网络访问范围。");
                    }
                });
            });
        });
        ui.add_space(6.0);
    }
    if let Some(index) = remove {
        profile.tunnels.remove(index);
    }
    if ui
        .add_enabled(
            profile.tunnels.len() < MAX_RULES,
            egui::Button::new("添加隧道规则"),
        )
        .clicked()
    {
        profile.tunnels.push(Rule::default());
    }
    if profile.tunnels.is_empty() {
        ui.weak("添加一条规则，配置监听地址、端口及访问目标。");
    }
    if let Err(error) = rshell_core::tunnel::validate(&profile.tunnels, profile.tunnel_only) {
        ui.colored_label(ui.visuals().error_fg_color, error.to_string());
    }
}

fn endpoint(ui: &mut egui::Ui, host: &mut String, port: &mut u16) {
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(host).desired_width(190.0));
        ui.label("端口");
        ui.add(egui::DragValue::new(port).range(1..=65535));
    });
}

pub fn status(ui: &mut egui::Ui, tab: &Tab) {
    if tab.profile.tunnels.is_empty() {
        return;
    }
    if !tab.owns_tunnels {
        ui.weak("此标签为复制的终端渠道；隧道由原始连接标签管理。");
        return;
    }
    egui::CollapsingHeader::new(if tab.profile.tunnel_only {
        "隧道连接"
    } else {
        "SSH 隧道状态"
    })
    .id_salt(("tunnels", tab.id))
    .default_open(true)
    .show(ui, |ui| {
        egui::ScrollArea::vertical()
            .id_salt(("tunnel_status", tab.id))
            .max_height(if tab.profile.tunnel_only {
                ui.available_height()
            } else {
                150.0
            })
            .show(ui, |ui| {
                for (index, rule) in tab.profile.tunnels.iter().enumerate() {
                    ui.label(
                        RichText::new(format!("{} · {}", index + 1, rule.kind.label())).strong(),
                    );
                    ui.label(rule.description());
                    let state = if !rule.enabled {
                        "已禁用"
                    } else if tab.finished {
                        "已停止"
                    } else {
                        tab.tunnel_statuses
                            .get(index)
                            .map(String::as_str)
                            .unwrap_or("等待连接")
                    };
                    ui.weak(state);
                    ui.add_space(4.0);
                }
            });
    });
    if tab.profile.tunnel_only {
        ui.weak("此会话不提供终端输入。使用工具栏断开或重新连接。");
    }
}
