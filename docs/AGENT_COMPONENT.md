# Agent 组件按需安装

主程序默认不打包 Python、SDK 或 Claude 引擎。使用「Agent · 会执行」且组件尚未就绪时弹出选择框：

- 下载并使用：从公开 GitHub Release 下载应用内固定版本，显示进度；完成大小和 SHA-256 校验、受限解压及 manifest 文件清单校验后，原子提交到用户缓存，再继续本次 Agent 请求。
- 选择本机组件：选择已有的 `rshell-agent-sdk.exe`，记住路径，本次直接使用；不能用普通 `claude.exe` 代替 SDK 桥接组件。
- 取消：不下载、不执行；普通终端和基础 AI 对话继续可用。

优先级为明确配置的 SDK 路径、应用旁的完整离线组件、当前版本用户缓存。都不存在时才询问；没有开机后台下载。缓存位于 `%LOCALAPPDATA%\RShell\components\<组件版本>`，主程序升级和卸载保留缓存。用户手动删除或破坏已安装目录时，可删除该版本缓存后重新选择下载，或指定离线组件。停止 Agent、切换或断开目标会取消选择或正在进行的下载；网络失败可重新发送原请求重试。

## 构建与发布

默认：`scripts/build.ps1 -Package` 和 `scripts/build-installer.ps1` 只构建轻量主程序。完整离线包显式增加 `-WithAgent`，输出文件名带 `full`。

独立组件：`scripts/package-agent-sdk.ps1 -Repository gooddayday009/rshell-components -ComponentVersion 0.2.174-r1`。此步骤需要 Python SDK 构建环境，生成 `dist/RShell-Agent-SDK-<版本>-windows-x64.zip`、SHA-256 文件，并更新 `agent-sdk/component.json`。主程序编译时内嵌该描述，不从远端读取可变的 latest 清单。必须发布精确匹配的 ZIP 并验证匿名下载后再交付主程序；组件变更应使用新版本，不覆盖已经发布的资产。

公开组件仓库只包含组件发布物及说明/许可资料；应用源码仓库保持私有。应用下载不携带模型 API Key 或 GitHub 登录凭据。HTTPS 连接超时 15 秒，总下载超时 10 分钟，最多 5 次 HTTPS 重定向。ZIP 上限 256 MiB、解压总量上限 768 MiB、最多 4096 项；拒绝目录越界、Windows 设备路径、符号链接、重复条目和未列入 manifest 的文件。取消或失败不启用半成品，已有可用组件不受影响。

组件的 Python、SDK、引擎与依赖遵循各自许可；许可和版本清单随组件 ZIP 一起提供。固定 SHA-256 是内容完整性校验，不等于代码签名或第三方许可审查完成。
