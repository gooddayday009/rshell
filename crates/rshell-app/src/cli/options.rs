use anyhow::{Context, Result, bail};
use std::{collections::HashSet, ffi::OsString, path::PathBuf};

pub const HELP: &str = "RShell CLI — 无界面 SSH / SFTP

默认关闭：请先在 RShell 设置 → 会话 中启用“启用命令行（CLI）”。
CLI 只主动连接目标服务器，不监听端口。此开关不能隔离同一 Windows 用户下的恶意程序。

用法：
  rshell-cli exec [连接参数] -- \"完整远端命令\"
  rshell-cli sftp ls [连接参数] [--json] -- <远端目录>
  rshell-cli sftp upload [连接参数] -- <本地文件> <远端文件>
  rshell-cli sftp download [连接参数] -- <远端文件> <本地文件>
  rshell-cli sftp sync upload [连接参数] [--dry-run] [--delete] [--json] -- <本地目录> <远端目录>
  rshell-cli sftp sync download [连接参数] [--dry-run] [--delete] [--json] -- <远端目录> <本地目录>

连接参数（二选一）：
  --profile <名称或UUID>         使用已保存 SSH 会话
  --host <主机> --user <用户名>  直接连接，可加 --port <1-65535>
  --identity <私钥路径>          直接连接时使用私钥
  --password-stdin               从标准输入第一行读取密码或私钥口令（UTF-8）
  --config-dir <目录>            优先于 RSHELL_CONFIG_DIR 和默认用户配置
  --timeout <秒>                 整个操作的超时，默认 300，范围 1-86400
  --dry-run                      仅预览目录同步差异，不写入同步目录
  --delete                       目录同步时显式删除目标端多余条目，默认保留
  -h, --help                     显示帮助
  -V, --version                  显示版本

exec 不分配 PTY，不转发 stdin，不自动拼接或转义命令；-- 后必须是一个完整参数。
SFTP upload/download 传输单文件且不覆盖；sync 递归按内容比较并更新同名文件。
sync 同步源目录的内容到目标根目录；远端根必须为绝对路径。失败可能保留 .part。
首次主机密钥自动保存，密钥变化阻断。已保存会话的隧道和空闲发送不启用。
密码不接受命令行参数值；不指定 --password-stdin 时尝试会话已有加密凭据。
退出码：远端命令原码；SFTP 成功 0；用法错误 2；超时 124；取消 130；其他错误 255。";

#[derive(Debug)]
pub enum Action {
    Help,
    Version,
    Exec(String),
    List(String),
    Upload {
        local: PathBuf,
        remote: String,
    },
    Download {
        remote: String,
        local: PathBuf,
    },
    Sync {
        local: PathBuf,
        remote: String,
        upload: bool,
    },
}
#[derive(Debug)]
pub struct Options {
    pub action: Action,
    pub profile: Option<String>,
    pub host: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity: Option<String>,
    pub password_stdin: bool,
    pub config_dir: Option<PathBuf>,
    pub timeout: u64,
    pub json: bool,
    pub dry_run: bool,
    pub delete: bool,
}
fn text(value: OsString) -> Result<String> {
    value
        .into_string()
        .map_err(|_| anyhow::anyhow!("参数必须是有效 Unicode"))
}
impl Options {
    pub fn parse(args: Vec<OsString>) -> Result<Self> {
        let mut options = Self {
            action: Action::Help,
            profile: None,
            host: None,
            user: None,
            port: None,
            identity: None,
            password_stdin: false,
            config_dir: None,
            timeout: 300,
            json: false,
            dry_run: false,
            delete: false,
        };
        if args.is_empty() || (args.len() == 1 && matches!(args[0].to_str(), Some("--help" | "-h")))
        {
            return Ok(options);
        }
        if args.len() == 1 && matches!(args[0].to_str(), Some("--version" | "-V")) {
            options.action = Action::Version;
            return Ok(options);
        }
        let mut args = args.into_iter();
        let command = text(args.next().unwrap())?;
        let operation = if command == "sftp" {
            text(
                args.next()
                    .context("sftp 需要 ls、upload、download 或 sync")?,
            )?
        } else if command == "exec" {
            command
        } else {
            bail!("未知命令，支持 exec 或 sftp");
        };
        if !matches!(
            operation.as_str(),
            "exec" | "ls" | "upload" | "download" | "sync"
        ) {
            bail!("未知 SFTP 操作");
        }
        let sync_upload = if operation == "sync" {
            match args.next().as_deref().and_then(|v| v.to_str()) {
                Some("upload") => true,
                Some("download") => false,
                Some("--help" | "-h") => return Ok(options),
                _ => bail!("sftp sync 需要 upload 或 download 方向"),
            }
        } else {
            false
        };
        let mut seen = HashSet::new();
        let mut positional = Vec::new();
        while let Some(arg) = args.next() {
            let flag = text(arg)?;
            if flag == "--" {
                positional.extend(args);
                break;
            }
            if flag == "--help" || flag == "-h" {
                return Ok(options);
            }
            if !seen.insert(flag.clone()) {
                bail!("参数不能重复");
            }
            if flag == "--password-stdin" {
                options.password_stdin = true;
                continue;
            }
            if flag == "--json" {
                options.json = true;
                continue;
            }
            if flag == "--dry-run" {
                options.dry_run = true;
                continue;
            }
            if flag == "--delete" {
                options.delete = true;
                continue;
            }
            if !matches!(
                flag.as_str(),
                "--profile"
                    | "--host"
                    | "--user"
                    | "--port"
                    | "--identity"
                    | "--config-dir"
                    | "--timeout"
            ) {
                // Do not echo unknown arguments: a misplaced password could be one.
                bail!("未知参数；命令和文件路径必须位于 -- 后");
            }
            let value = args.next().context("参数缺少值")?;
            if value.to_str().is_some_and(|s| s.starts_with("--")) {
                bail!("参数缺少值");
            }
            if flag == "--config-dir" {
                if value.is_empty() {
                    bail!("配置目录不能为空");
                }
                options.config_dir = Some(value.into());
                continue;
            }
            let value = text(value)?;
            if value.is_empty() {
                bail!("参数值不能为空");
            }
            match flag.as_str() {
                "--profile" => options.profile = Some(value),
                "--host" => options.host = Some(value),
                "--user" => options.user = Some(value),
                "--identity" => options.identity = Some(value),
                "--port" => {
                    options.port = Some(value.parse().context("端口必须为 1-65535")?);
                    if options.port == Some(0) {
                        bail!("端口必须为 1-65535");
                    }
                }
                "--timeout" => {
                    options.timeout = value.parse().context("超时必须为 1-86400 秒")?;
                    if !(1..=86400).contains(&options.timeout) {
                        bail!("超时必须为 1-86400 秒");
                    }
                }
                _ => unreachable!(),
            }
        }
        if options.profile.is_some() {
            if options.host.is_some()
                || options.user.is_some()
                || options.port.is_some()
                || options.identity.is_some()
            {
                bail!("--profile 不能与直接连接参数混用");
            }
        } else if options.host.is_none() || options.user.is_none() {
            bail!("需要 --profile，或同时提供 --host 与 --user");
        }
        if options.json && !matches!(operation.as_str(), "ls" | "sync") {
            bail!("--json 仅用于 sftp ls 或 sync");
        }
        if (options.dry_run || options.delete) && operation != "sync" {
            bail!("--dry-run 和 --delete 仅用于 sftp sync");
        }
        let expected = if matches!(operation.as_str(), "exec" | "ls") {
            1
        } else {
            2
        };
        if positional.len() != expected {
            bail!("-- 后的参数数量不正确；exec 接收一个完整命令，传输接收源和目标路径");
        }
        if positional
            .iter()
            .any(|p| p.is_empty() || p.to_string_lossy().contains('\0'))
        {
            bail!("命令或路径不能为空或包含 NUL");
        }
        let mut positional = positional.into_iter();
        let first = positional.next().unwrap();
        options.action = match operation.as_str() {
            "exec" => {
                let command = text(first)?;
                if command.trim().is_empty() || command.len() > 65536 {
                    bail!("命令不能为空，且最多 65536 UTF-8 字节");
                }
                Action::Exec(command)
            }
            "ls" => Action::List(text(first)?),
            "upload" => Action::Upload {
                local: first.into(),
                remote: text(positional.next().unwrap())?,
            },
            "download" => Action::Download {
                remote: text(first)?,
                local: positional.next().unwrap().into(),
            },
            "sync" if sync_upload => Action::Sync {
                local: first.into(),
                remote: text(positional.next().unwrap())?,
                upload: true,
            },
            "sync" => Action::Sync {
                remote: text(first)?,
                local: positional.next().unwrap().into(),
                upload: false,
            },
            _ => unreachable!(),
        };
        Ok(options)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(args: &[&str]) -> Result<Options> {
        Options::parse(args.iter().map(OsString::from).collect())
    }
    #[test]
    fn command_and_paths_are_not_requoted_or_parsed_as_options() {
        let options = parse(&[
            "exec",
            "--profile",
            "测试",
            "--",
            "printf '%s' \"a b\"; exit 7",
        ])
        .unwrap();
        assert!(matches!(options.action, Action::Exec(s) if s == "printf '%s' \"a b\"; exit 7"));
        let options = parse(&[
            "sftp",
            "upload",
            "--host",
            "::1",
            "--user",
            "u",
            "--",
            "C:\\a b.txt",
            "--remote",
        ])
        .unwrap();
        assert!(matches!(options.action, Action::Upload { remote, .. } if remote == "--remote"));
    }
    #[test]
    fn reject_ambiguous_or_unsafe_invocations() {
        for args in [
            vec!["exec", "--profile", "p", "--host", "h", "--", "id"],
            vec!["exec", "--host", "h", "--", "id"],
            vec!["exec", "--profile", "p", "--port", "0", "--", "id"],
            vec!["exec", "--profile", "p", "--timeout", "0", "--", "id"],
            vec!["exec", "--profile", "p", "--", "echo", "lost quoting"],
            vec!["exec", "--profile", "p", "--password", "secret", "--", "id"],
            vec!["exec", "--profile", "p", "--profile", "q", "--", "id"],
            vec!["exec", "--profile", "p", "--json", "--", "id"],
            vec!["sftp", "upload", "--profile", "p", "--", "only-one"],
            vec![
                "sftp",
                "download",
                "--profile",
                "p",
                "--delete",
                "--",
                "/a",
                "a",
            ],
            vec!["sftp", "ls", "--profile", "p", "--dry-run", "--", "/"],
            vec!["sftp", "sync", "--profile", "p", "--", "a", "/a"],
        ] {
            assert!(parse(&args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn directory_sync_direction_and_delete_are_explicit() {
        let options = parse(&[
            "sftp",
            "sync",
            "upload",
            "--profile",
            "p",
            "--",
            "D:\\source",
            "/target",
        ])
        .unwrap();
        assert!(!options.delete && !options.dry_run);
        assert!(matches!(options.action, Action::Sync { upload: true, .. }));
        let options = parse(&[
            "sftp",
            "sync",
            "download",
            "--profile",
            "p",
            "--dry-run",
            "--delete",
            "--json",
            "--",
            "/source",
            "D:\\target",
        ])
        .unwrap();
        assert!(options.delete && options.dry_run && options.json);
        assert!(
            matches!(options.action, Action::Sync { local, remote, upload: false } if remote == "/source" && local == std::path::Path::new("D:\\target"))
        );
    }
}
