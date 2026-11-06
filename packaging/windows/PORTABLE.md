# RShell Windows 便携测试版

脚本自动化使用同目录的 `rshell-cli.exe`，默认关闭，需在“设置 → 会话”启用。支持 SSH 命令、SFTP 单文件上传/下载及 `sftp sync upload/download` 目录同步，不依赖系统 OpenSSH。执行 `./rshell-cli.exe --help` 或阅读 `docs/CLI.md`。目录同步默认保留目标多余文件，只有显式 `--delete` 才删除，可先用 `--dry-run` 预览；CLI 不监听网络端口、不自动加入 PATH。

解压整个 ZIP 后双击 `rshell.exe` 启动，无需安装 Rust。轻量包不含 Agent 组件；首次执行型 Agent 使用时可选择下载或使用本机组件。full 离线包的组件位于旁边的 `agent-sdk` 目录，请保留完整目录。两种方式均无需另外安装 Python 或 CLI。Agent 仍需要有效的 API 认证配置。

启动后主区域默认显示本地 PowerShell，可直接输入 `ssh root@主机`。命令行 SSH 需要系统安装 OpenSSH 客户端；其主机记录与内置会话独立。

也可以使用内置 SSH：

1. 点击「新建会话」，填写有权限访问的 SSH 测试服务器。
2. 选择密码或私钥认证，点击「保存并连接」。
3. 首次连接自动记录主机密钥并进入终端；可在设置中查看指纹，已记录密钥变化时连接会阻断。

默认配置位于当前 Windows 用户目录，可在应用「设置」中查看准确位置。删除便携程序目录不会删除用户配置或已保存的 Windows 凭据。不要在不清楚用途时清理这些数据。

这是未签名的开发测试版，不是已完成验收的正式安装包。未完成项目见 `docs/STATUS.md`。包内根 README 面向源码开发；其中 Cargo 命令需要完整源代码和 Rust 工具链，不是在此便携目录内执行。


2.3.0 起默认 ZIP 为轻量包。直接运行 rshell.exe 即可使用终端与普通 AI 对话；执行型 Agent 缺少组件时先弹框，选择下载后才下载到当前用户目录并显示进度，也可选择本机已有 rshell-agent-sdk.exe。离线使用请选择 full 完整包或在 Agent 设置指定组件路径。
