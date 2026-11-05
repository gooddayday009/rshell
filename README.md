# RShell

RShell 是面向 Windows x64 的原生终端与 SSH 客户端，使用 Rust 开发，集成多会话管理、文件传输和 AI 助手，适合日常服务器运维与本地命令行操作。

## 主要功能

- **多种连接**：SSH、本地 PowerShell、Telnet 和串口，支持 JumpServer 堡垒机资产同步与连接。
- **会话管理**：文件夹分组、多标签、终端分屏，以及 RShell / Xshell 会话导入导出。导出直接保存全部已保存会话，无需选择会话；Xshell 格式导出全部 SSH/Telnet 会话，跳过串口。导入的空用户名会话可原样导出，连接前需补齐用户名。
- **文件传输**：内置 SSH 会话支持 SFTP 和 ZMODEM（sz / rz）。
- **AI 助手**：围绕终端上下文进行问答、命令生成与解释、报错排查；建议命令可编辑，执行前需逐次确认。
- **个性化界面**：深浅主题、字体与字号设置、终端缩放和历史回看。
- **密钥与更新**：SSH 主机密钥校验、用户密钥管理，以及应用内更新检查与下载校验。

## 下载使用

从 [GitHub Releases](https://github.com/gooddayday009/rshell/releases) 下载 Windows x64 安装包或便携 ZIP。支持 Windows 10 1809、Windows 11 及更新系统。

启动后可直接使用本地 PowerShell，也可新建远端会话。远端执行 `exit` 后，Shell 标签保留原连接信息。点击顶部工具栏的“重新连接”会在原标签连接同一目标。AI 功能需配置模型接口；Agent 组件按需下载。

## 更多资料

- [快捷键](docs/SHORTCUTS.md)
- [项目架构](docs/ARCHITECTURE.md)
- [JumpServer 集成方案](docs/JumpServer集成方案.md)
- [Agent 组件说明](docs/AGENT_COMPONENT.md)

## 许可

RShell 自有代码采用 [PolyForm Noncommercial License 1.0.0](LICENSE)，允许非商业用途的使用、修改和分发；商业使用需另行取得授权。第三方依赖与字体遵循各自许可证。
