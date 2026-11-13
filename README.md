# RShell

RShell 是面向 Windows x64 的原生终端与 SSH 客户端，使用 Rust 开发，集成多会话管理、文件传输和 AI 助手，适合日常服务器运维与本地命令行操作。  

<img width="1398" height="806" alt="PixPin_2026-11-03_15-35-45" src="https://github.com/user-attachments/assets/d17b3745-6815-43b0-b389-041f2d7017b8" />


## 主要功能

Windows 安装包和便携包必须同时保留 `rshell.exe`、`rshell-cli.exe` 和 `rshell-shell.exe`。内置 Shell 由同目录的 `rshell-shell.exe` 提供；出现“缺少内置 Shell 组件”时，请重新安装包含该组件的完整 RShell 安装包。

Windows 内置 Shell 提示符使用本机目录格式，例如 `RShell C:\Users\Administrator\>`，切换目录后自动更新；命令仍使用内置 Shell 的 Bash 兼容语法。
内置 `ls` 无参数时列出当前 Shell 目录，`ls .`、子目录和 `..` 同样相对当前目录解析。

- **多种连接**：SSH、本地 Bash / PowerShell、Telnet 和串口，支持 JumpServer 堡垒机资产同步与连接。
- **命令行 CLI**：默认关闭，需在“设置 → 会话”显式启用。`rshell-cli.exe` 可无界面执行 SSH 命令、列出 SFTP 目录、上传/下载单文件及递归目录同步。目录同步按内容比较，支持差异预览，默认保留目标端多余文件，可显式选择镜像删除；CLI 不开放网络监听端口。认证、退出码和安全边界见 [CLI 使用说明](docs/CLI.md)。
- **SSH 隧道**：本地端口转发、远程端口转发和 SOCKS5 动态代理；支持仅隧道连接，规则随会话保存，显示监听状态及最近连接错误。通过“文件 → 新建 SSH 隧道连接”或会话设置中的“SSH 隧道”配置，详见[隧道使用说明](docs/TUNNELS.md)。
- **会话管理**：文件夹分组、多标签、终端分屏，以及 RShell / Xshell 会话导入导出。文件夹使用随主题变化的 Lucide 展开/收起图标；文件夹和会话均可整行拖动，拖到行的上/下边缘排序、文件夹中部移入、“所有会话”移回顶层，顺序自动保存，支持混合排列。导出直接保存全部已保存会话，无需选择会话；Xshell 格式导出全部 SSH/Telnet 会话，跳过串口。导入的空用户名会话可原样导出，连接前需补齐用户名。
- **文件传输**：内置 SSH 会话支持 SFTP 和 ZMODEM（sz / rz）。SFTP 使用独立原生窗口，可拖动到主窗口外或其他显示器，独立缩放且不阻塞主窗口操作；左侧为远程目录、右侧默认打开系统桌面（支持重定向桌面），下方显示传输队列；中间和下方分隔条可拖动调整比例，文件夹以主题适配的金色图标和文件夹标记区分，文件显示类型与大小。点击图标、名称、大小或行内空白均可整行选中高亮；左右两栏各有文件名查找框，在当前目录内筛选（支持中文、不区分大小写），清除后恢复全部条目。队列用文字和彩色状态标签区分任务，可按进行中、已完成、失败、已暂停筛选；两侧文件或文件夹右键可重命名、删除；重命名不覆盖同名目标，删除必须在显示主机及完整路径的弹窗中确认，递归删除文件夹且不进入回收站。双击打开文件夹，选中后上传/下载，也可跨栏拖动文件或文件夹、从资源管理器拖入远程栏。文件夹递归传输，任务可暂停和校验后续传。同名文件不覆盖；续传记录限当前会话，关闭面板后台继续，关闭会话或退出程序后不保留。
- **AI 助手**：围绕终端上下文进行问答、命令生成与解释、报错排查；建议命令可编辑，执行前需逐次确认。
- **个性化界面**：深浅主题、字体与字号设置、终端缩放和历史回看；滚轮与触控板按平滑位移累计滚动。终端内 Ctrl+V 发送给远端应用（如 Vim 可视块模式），粘贴使用 Ctrl+Shift+V 或 Shift+Insert。
- **密钥与更新**：SSH 主机密钥校验、用户密钥管理，以及应用内更新检查与下载校验；更新窗口的 GitHub 发布页链接通过系统默认浏览器打开。
  - SSH 会话设置选择「私钥」认证后，可在「用户密钥」下拉框选择已生成或导入的密钥，也可继续浏览普通私钥文件。托管密钥无需填写口令；列表支持后台加载与手动刷新。管理器支持点击整行选中密钥，悬停查看指纹。

## macOS 源码支持

2.3.20 的 Apple Silicon / Intel macOS CI 测试与 `.app` 打包已通过，真实输入法与安装体验仍待人工验收。构建、凭据迁移及验收边界见 [macOS 指南](docs/MACOS.md)。Windows 专用 Agent 组件在 macOS 隐藏，保留通用 AI 对话与命令建议。

## 下载使用

从 [GitHub Releases](https://github.com/gooddayday009/rshell/releases) 下载 Windows x64 安装包或便携 ZIP。支持 Windows 10 1809、Windows 11 及更新系统。

启动页和“+”默认在 RShell 标签内运行 **Git Bash**，可在“设置 → 会话 → 默认 Shell”选择 PowerShell。需要本机安装 Git for Windows（标准安装目录或 Git 的 `cmd` 目录已加入 PATH）；找不到时显示安装提示，不会启动 WSL 或静默切换其他 Shell。支持真实 Bash 的变量、引用、数组、函数、循环、命令替换、管道、重定向和脚本，以及 Git 自带的 `ls`、`grep`、`sed`、`awk`、`find` 等工具。Windows 盘符使用 `/c/`、`/d/` 路径；这不是 Linux 内核环境，不提供 `systemd` 或 Linux 软件包管理器。

Bash 默认以 `--noprofile --norc -i` 启动，避免启动脚本拖慢首次输入；需要自定义配置时可手动 `source ~/.bashrc`。支持文件名 Tab 补全、上下键命令历史和 Ctrl+C 中断；默认会话历史最多 1000 条，不写入磁盘。所有本地命令作用于 Windows 本机文件。原来的 `connect` / `reconnect` 提示符命令已由真实 Bash 替代，建立 SSH 使用连接工具栏或会话列表。

远端断开后，按回车在原标签进入默认本地 Shell，旧画面保留在滚动历史中；工具栏“重新连接”仍连接原远端目标。远端执行 `exit`、手动断开或异常结束均保留记录；“清除滚动缓冲区，当连接关闭”默认关闭，主动开启才清空。历史仍受容量上限约束，关闭标签时释放。AI 功能需配置模型接口；Agent 组件按需下载。

退出确认框使用分隔清晰的标题、提示和右对齐按钮，颜色跟随深浅主题。开发时可用 `--demo --smoke-test --smoke-confirm-exit` 单独检查布局，配置与截图步骤见 [本地测试指南](docs/LOCAL_TESTING.md)。

## 更多资料

- [快捷键](docs/SHORTCUTS.md)
- [会话树排序与移动](docs/SESSION_TREE.md)
- [命令行 CLI](docs/CLI.md)
- [项目架构](docs/ARCHITECTURE.md)
- [SSH 隧道](docs/TUNNELS.md)
- [JumpServer 集成方案](docs/JumpServer集成方案.md)
- [Agent 组件说明](docs/AGENT_COMPONENT.md)
- [GitHub 自动打包与发布](docs/RELEASING.md)

## 许可

RShell 自有代码采用 [PolyForm Noncommercial License 1.0.0](LICENSE)，允许非商业用途的使用、修改和分发；商业使用需另行取得授权。第三方依赖与字体遵循各自许可证。
