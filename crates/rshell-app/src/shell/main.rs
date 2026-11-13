//! Bundled console host; shell semantics come from the pinned rubash engine.
use rubash::api::{ExternalCommandHandler, HostExternalCommandOutput, RunOptions, Session};
use std::collections::HashMap;

struct Commands;
impl ExternalCommandHandler for Commands {
    fn handle_command(
        &mut self,
        argv: &[String],
        env: &HashMap<String, String>,
    ) -> Option<HostExternalCommandOutput> {
        let command = argv.first()?.as_str();
        let result = match command {
            "ssh" => rshell_core::shell_bridge::SshTarget::parse(&argv[1..])
                .and_then(rshell_core::shell_bridge::request_ssh)
                .map(|()| "请在 RShell 连接窗口完成认证。\n".to_owned()),
            "ls" => list(&argv[1..], env),
            _ => return None,
        };
        Some(match result {
            Ok(text) => HostExternalCommandOutput {
                stdout: text.into_bytes(),
                stderr: Vec::new(),
                status: 0,
            },
            Err(error) => HostExternalCommandOutput {
                stdout: Vec::new(),
                stderr: format!("{command}: {error:#}\n").into_bytes(),
                status: 1,
            },
        })
    }
}

fn list(args: &[String], env: &HashMap<String, String>) -> anyhow::Result<String> {
    let mut all = false;
    let mut long = false;
    let mut paths = Vec::new();
    for arg in args {
        if arg.starts_with('-') {
            for flag in arg.chars().skip(1) {
                match flag {
                    'a' | 'A' => all = true,
                    'l' => long = true,
                    'h' | '1' => {}
                    _ => anyhow::bail!("支持 ls [-alh] [目录或文件]"),
                }
            }
        } else {
            paths.push(arg.as_str());
        }
    }
    if paths.is_empty() {
        paths.push(".");
    }
    let mut output = String::new();
    for path in paths {
        let path = Session::resolve_shell_path_from_env(path, env);
        let path = if path.is_relative() {
            let pwd = env
                .get("PWD")
                .ok_or_else(|| anyhow::anyhow!("当前 Shell 目录不可用"))?;
            Session::resolve_shell_path_from_env(pwd, env).join(path)
        } else {
            path
        };
        let mut entries = if path.is_dir() {
            std::fs::read_dir(&path)?
                .take(10001)
                .map(|entry| entry.map(|e| e.path()))
                .collect::<Result<Vec<_>, _>>()?
        } else {
            vec![path]
        };
        if entries.len() > 10000 {
            anyhow::bail!("目录超过 10000 项，请缩小范围");
        }
        entries.sort();
        for entry in entries {
            let name = entry.file_name().unwrap_or_default().to_string_lossy();
            if !all && name.starts_with('.') {
                continue;
            }
            let metadata = std::fs::symlink_metadata(&entry)?;
            if long {
                output.push_str(&format!(
                    "{} {:>12} ",
                    if metadata.is_dir() { 'd' } else { '-' },
                    metadata.len()
                ));
            }
            output.push_str(&name);
            if metadata.is_dir() {
                output.push('/');
            }
            output.push('\n');
            if output.len() > 1024 * 1024 {
                anyhow::bail!("ls 输出超过 1 MiB");
            }
        }
    }
    Ok(output)
}

fn prompt_template() -> &'static str {
    if cfg!(windows) {
        "RShell ${__rshell_prompt_dir}> "
    } else {
        r"RShell \w\$ "
    }
}

fn run() -> anyhow::Result<i32> {
    if std::env::args().any(|arg| arg == "--license") {
        print!("{}", include_str!("../../assets/licenses/rubash-MIT.txt"));
        return Ok(0);
    }
    let mut session = Session::builder()
        .argv0("rshell-shell")
        .interactive(true)
        .inherit_process_stdin(true)
        .env("PS1", prompt_template())
        .env(
            "PROMPT_COMMAND",
            if cfg!(windows) {
                "__rshell_prompt_path"
            } else {
                ""
            },
        )
        .env("HISTFILE", "")
        .env("HISTSIZE", "1000")
        .external_command_handler(Box::new(Commands))
        .build()
        .map_err(|error| anyhow::anyhow!("{error:?}"))?;
    // The pinned engine provides native implementations of cat/grep/head/sed/wc.
    if cfg!(windows) {
        session
            .run(
                r#"__rshell_prompt_path() { local p="$PWD" d; if [[ "$p" == /[a-zA-Z]/* ]]; then d="${p:1:1}"; p="${d^^}:${p:2}"; fi; p="${p//\//\\}"; __rshell_prompt_dir="${p%\\}\\"; }"#,
                &RunOptions::default(),
            )
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
    }
    session
        .executor_mut()
        .set_external_file_builtins_enabled(true);
    let result = session
        .run_interactive_stdin()
        .map_err(|error| anyhow::anyhow!("{error:?}"))?;
    let status = result.status;
    let _ = session.close();
    Ok(status)
}

fn main() {
    let status = match run() {
        Ok(status) => status,
        Err(error) => {
            eprintln!("RShell 内置 Shell: {error:#}");
            1
        }
    };
    std::process::exit(status);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_uses_shell_directory_for_relative_paths() {
        let root = tempfile::tempdir().unwrap();
        let current = root.path().join("中文 space");
        std::fs::create_dir(&current).unwrap();
        std::fs::create_dir(current.join("child")).unwrap();
        std::fs::write(current.join("here.txt"), "here").unwrap();
        std::fs::write(current.join("child/nested.txt"), "nested").unwrap();
        let env = HashMap::from([("PWD".into(), current.to_string_lossy().into_owned())]);
        assert_eq!(list(&[], &env).unwrap(), "child/\nhere.txt\n");
        assert_eq!(list(&[".".into()], &env).unwrap(), "child/\nhere.txt\n");
        assert_eq!(list(&["child".into()], &env).unwrap(), "nested.txt\n");
        assert_eq!(
            list(&["child/..".into()], &env).unwrap(),
            "child/\nhere.txt\n"
        );
        assert_eq!(
            list(&[current.to_string_lossy().into_owned()], &env).unwrap(),
            "child/\nhere.txt\n"
        );
    }
}
