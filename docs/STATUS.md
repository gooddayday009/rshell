# 开发状态

- 2026-11-13：按用户要求，macOS CI 保留常规与凭据测试、双架构打包，移除 `--smoke`；窗口冒烟不在本次验证范围。

## macOS Shell 控制通道读取修复（2026-11-13）

- 失败证据：GitHub Actions run `43376387149` 的 macOS job `131723745557`，正确令牌接收测试失败。
- 已实现：接受回环连接后显式恢复阻塞模式，保留 100 ms 读取超时与 2048 字节上限，避免 macOS/BSD 继承监听器非阻塞模式；新增延迟分段请求回归测试。
- 待验证：macOS runner 修复后重跑。本机 Windows 检查不代表 macOS 验收；未改动认证规则。

## 标签右键快捷栏入口（2026-11-13）

- 已实现：终端标签右键添加/移除快捷栏，复用现有后台配置保存逻辑；本地终端、JumpServer 标签及未保存会话禁用入口。
- 验证范围：针对性 GUI 测试检查右键菜单、点击添加、持久化及移除；不运行冒烟测试。用户安装后的交互验收仍开放。

## 内置 ls 当前目录修复（2026-11-13）

- 已实现：`ls` 的相对路径以当前 Shell 的 `PWD` 为基准，不再使用程序启动目录；保留绝对路径解析。
- 验证范围：回归测试覆盖无参数、`.`、子目录、`..`、绝对路径及中文空格目录；检查日志为 `target/ls-directory-checks.log`。按用户要求不运行冒烟测试。
- 待完成：用户安装后的交互验收。

## Windows 内置 Shell 提示符（2026-11-13）

- 已实现：提示符去掉引擎名称，Windows 下使用大写盘符、反斜杠与尾部目录分隔符，例如 `RShell C:\Users\Administrator\>`；每次提示符显示前根据当前目录更新，不修改 Shell 内部路径与命令语法。
- 已验证：本地进程切换目录后提示符同步更新；窗口及打包验证结果见本次交付记录。
- 待完成：用户安装后的交互验收。

## Windows 内置 Shell 打包修复（2026-11-13）

- 已实现：Release 显式构建全部二进制并检查必需文件；安装器与便携包收录同目录的 `rshell-shell.exe` 和 rubash MIT 许可证，卸载时清理该程序。修复此前已编译组件但安装包未收录的问题。
- 已验证：NSIS 安装包解压后三个程序的 SHA-256 与 Release 产物一致；包内 Shell 执行命令成功；使用独立测试配置启动包内主程序，内置 Shell 提示符正常显示，无缺少组件错误。截图：`target/test-artifacts/package-shell-fix.png`。
- 待完成：用户安装目录的覆盖升级与安装/卸载验收；解压后验证不代表安装验收完成。

## main 用户密钥选择与管理器（2026-11-10）

- 已实现（纯 Rust / egui）：管理器整行选择、指纹提示、全宽表格、对齐表单和窗口滚动；刷新后移除失效选择。SSH 会话增加托管密钥下拉框，后台加载、刷新与错误提示；选择时清除原密码/口令及旧凭据身份，保留普通私钥文件入口。
- 已验证：原生深浅主题窗口检查；egui 指针事件回归覆盖点击算法单元格选中整行、会话下拉选择后路径绑定与旧凭据清除。完整检查结果见 `target/key-manager-checks.log`。
- 待完成：真实 SSH 服务器的托管密钥登录和用户人工验收；本次未提交、未发布。

## macOS 2.3.20（2026-11-06）

- 已提交并推送平台适配和两处跨平台测试修复，构建源码 `ba16f823c787ef7d5b844ed03e1f058a6bc73c0a`。
- [CI 42177646857](https://github.com/gooddayday009/rshell/actions/runs/42177646857)：Apple Silicon / Intel 均通过编译、严格 Clippy、常规测试、4 项钥匙串专项和 24 个窗口冒烟场景，完成 Release `.app` 打包和签名检查。
- 本地 `dist/RShell-2.3.20-macos-arm64.zip`、`dist/RShell-2.3.20-macos-x64.zip` 已核对 SHA-256、版本号及主程序/CLI 架构。Windows `scripts/test.ps1 -WithCredentials -Smoke` 通过。
- 保持开放：Developer ID 签名、公证、真实服务器与物理串口、真实输入法、全屏应用、性能和用户安装/升级验收。未发布正式 GitHub Release。详见 [macOS 指南](MACOS.md)。

## 2.3.14 PowerShell Unicode 测试修复（2026-11-06）

- 证据：GitHub `v2.3.12` 已通过 Rust/NSIS 初始化，但本地 PowerShell Unicode 测试输出为 `??????`。本机强制控制台代码页 437 后复现相同失败，日志 `target/test-artifacts/local-cp437-repro.log`。
- 已修复：测试先设置并确认代码页 437，再为 `[Console]::WriteLine` 显式设置 UTF-8，保留中文输入输出、尺寸调整、OpenSSH 输出和退出检查，避免依赖测试机的默认区域设置。只调整测试环境，不改应用启动或全局编码配置，也不跳过测试。
- 已验证：强制代码页 437 后的修复测试通过，日志 `target/test-artifacts/local-cp437-fixed.log`；本机 `scripts/test.ps1` 的格式检查、严格 Clippy 和 270 项常规测试通过（5 项专项默认忽略），日志 `target/test-artifacts/release-2.3.13-check.log`；分离并行改动后的 `2.3.14` 隔离快照也通过完整常规回归，日志 `target/test-artifacts/release-2.3.14-isolated-check.log`。

- 发布修正：`v2.3.13` 构建已取消，未发布；提交时混入的并行 macOS 本地 Shell 改动已从发布树分离，并保留在开发工作区。使用隔离源码快照验证 `v2.3.14`，避免并行修改影响验收。

- 云端验收：[GitHub Actions #4](https://github.com/gooddayday009/rshell/actions/runs/42173296792) 全部成功，包括常规测试、便携包、安装包及发布。[v2.3.14 Release](https://github.com/gooddayday009/rshell/releases/tag/v2.3.14) 已公开，四个附件齐全；安装包及便携包已匿名下载，文件大小和 SHA256 均匹配。保持未签名，真实安装升级等人工验收仍开放。

## 2.3.12 自动发布修复（2026-11-06）

- 已修复：Rust 安装命令分别传入 `--component rustfmt --component clippy`，避免 `clippy` 被当作工具链名导致 GitHub Actions 初始化失败。
- 已验证：本机使用已有 stable 工具链执行修正后的组件安装参数成功（加 `--no-update --no-self-update`，未更新本机工具链）。
- 本机 `scripts/test.ps1` 通过：格式检查、严格 Clippy、270 项常规测试（5 项专项默认忽略）；日志 `target/test-artifacts/release-2.3.12-check.log`。本次仅修复发布配置和版本号，未重新执行窗口冒烟。
- 发布策略：保留失败的 `v2.3.11` Tag，使用新版本 `v2.3.12` 触发修正后的工作流。GitHub 托管环境继续只执行常规测试，不执行窗口冒烟；实际云端构建与发布结果需单独确认。

## GitHub 自动发布（2026-11-06）

- 已实现：推送与 workspace 版本一致的稳定版本 `vX.Y.Z` Tag 后，在 Windows runner 运行格式检查、严格 Clippy 和常规测试，构建轻量安装包和便携包，校验 SHA256 并上传四个附件，最后公开 Release。按用户要求，GitHub 托管环境不运行窗口冒烟测试。发布流程见 [RELEASING.md](RELEASING.md)。
- 已验证：工作流 YAML 和内嵌 PowerShell 语法检查通过，确认托管流程不含 `-Smoke`。本机 `scripts/test.ps1 -Smoke` 通过：格式检查、严格 Clippy、270 项测试和 24 个窗口场景，5 项专项默认忽略；日志为 `target/test-artifacts/github-release-check.log`。本机结果不代表 GitHub runner 已通过。
- 待完成：首次 GitHub Actions 运行、实际 Release 上传与匿名下载验证。配置落地不代表线上发布成功；安装包仍未签名，人工兼容性和安装验收继续开放。

本文件记录 2026-11-06 的增量开发，不重新认定其他历史功能的完成状态。当前检出目录未提供规范引用的 `Rust重写Xshell工具方案.md`，未改写或降低其中验收要求。

## 2.3.11 SFTP 重命名与删除（2026-11-06）

- 已实现：两侧文件列表右键可重命名、删除文件或文件夹；重命名拒绝覆盖同名目标。删除须点击确认框中的“确认删除”，显示本地/远程主机和完整路径，并提示递归永久删除、不进入回收站；取消或关闭确认框不执行。磁盘和网络操作在后台运行，成功刷新对应目录，失败提示检查部分完成状态。
- 远端删除不主动跟随符号链接，目录分页读取、队列和总操作数有上限，整体超时 120 秒；遇到限制、断线或权限错误时可能部分完成，需刷新检查。并发修改远端目录的竞态不属于安全隔离保证。
- 已验证：格式与严格 Clippy、270 项测试通过（0 失败、5 项专项默认忽略）。新增覆盖确认前不操作、取消保留文件、确认后删除，以及深浅主题 580×480 确认框；本地和回环 SFTP 覆盖重命名、重名拒绝覆盖、非法名称拒绝和非空目录删除。测试使用 `RUST_TEST_THREADS=2`，避免既有会话树测试在高并发下超时。
- Release 构建通过，版本 2.3.11。`scripts/test.ps1 -Smoke` 全部通过，包含 24 个窗口场景；日志：`target/test-artifacts/package-2.3.11-check.log`。交付安装版 `dist/RShell-2.3.11-windows-x64-setup.exe` 和便携版 `dist/RShell-2.3.11-windows-x64-portable.zip`，附 SHA-256。原生截图只捕获主窗口，不能用作 SFTP 独立子窗口视觉验收；此处 SFTP 确认交互以 egui 指针测试为依据。
- 待完成：真实 Linux/OpenSSH、安装升级、真实服务器鼠标操作及 SFTP 子窗口人工视觉验收。未签名、未发布。

## 2.3.10 本地打包验证（2026-11-06）

- 本版汇总 CLI（默认关闭）、SFTP 目录同步与列表查找、会话树排序拖动、退出确认框布局；安装器与便携包包含 GUI、CLI 和使用说明。
- `scripts/test.ps1 -Smoke` 通过：格式、严格 Clippy、268 项测试通过、0 失败、5 项专项默认忽略，以及全部 24 个窗口场景。日志：`target/test-artifacts/package-2.3.10-check.log`。本次未修改凭据存储后端，未重跑凭据专项。
- Release 构建通过，CLI 输出 `rshell-cli 2.3.10`；Release 会话树深浅主题 800×520 窗口、180 px 侧栏截图检查通过，截图为 `target/test-artifacts/release-2.3.10-tree-{dark,light}.png`。
- 交付路径为 `dist/RShell-2.3.10-windows-x64-setup.exe` 和同版本便携 ZIP，附 SHA-256；本版打包取代下方各开发阶段的“未重新打包”状态，历史验证记录保持原样。
- 未签名、未发布；真实安装升级、Linux/OpenSSH、真实鼠标与多 DPI 等人工验收仍开放。当前缺失原方案文件，便携包不包含该文件。

## 退出确认框布局（2026-11-06）

- 已实现：320 px 内容宽度、24 px 内边距、分离的标题与提示、右对齐的 36 px 高按钮；确认按钮使用当前主题的强调色。退出确认条件与断开逻辑保持原样。
- 已验证：格式检查、GUI 二进制严格 Clippy 和开发构建通过；单独运行全工作区测试，264 项通过、5 项专项默认忽略，日志为 `target/test-artifacts/exit-dialog-tests.log`。深浅主题下的 800×520 原生窗口、180 px 窄侧栏截图通过检查：`target/test-artifacts/exit-dialog-dark.png`、`exit-dialog-light.png`。
- 检查限制：已运行 `scripts/test.ps1`，被已有 `crates/rshell-app/src/cli/options.rs:360` 的 `clippy::cmp_owned` 警告阻挡，不能记为整套脚本通过；日志为 `target/test-artifacts/exit-dialog-check.log`。本次未修改 CLI 文件，未打包或发布，截图验证不替代真实连接退出操作验收。

## SSH 隧道：已实现

- SSH 本地/远程端口转发、回环 SOCKS5 TCP 代理，以及无需 PTY/Shell 的仅隧道连接。
- 会话规则编辑、启用/禁用、配置持久化、`.rsh` 导入导出、状态和错误展示。
- 主机密钥与认证复用、后台 I/O、有界连接/队列、启动失败回滚、会话取消清理、仅隧道标签的输入及 AI 执行隔离。

## 已验证

- 7 项本机 russh 集成测试：本地/远程转发、128 KiB 二进制数据、TCP 半关闭、并发传输、断开清理和重连、SOCKS5 远端域名解析及协议拒绝、端口冲突/远程请求拒绝回滚、未请求远程渠道拒绝、目标连接失败状态、认证失败/主机密钥变化阻断、远端断开及复制渠道的独立生命周期。
- 配置校验、旧配置兼容、规则往返序列化、`.rsh` 导入导出兼容、仅隧道标签阻止终端及 AI 命令输入的测试通过。
- `scripts/test.ps1` 通过：格式检查、Clippy（警告作为错误）、240 项测试通过、0 失败、4 项既有专项测试默认忽略。日志在 `target/test-artifacts/tunnels-workspace-check.log`。本次未改变凭据存储，未启用 `-WithCredentials`。
- 原生窗口验证：深浅主题隧道配置窗口，以及 800×520 窗口、180 px 窄侧栏下的隧道状态；截图位于 `target/test-artifacts/tunnels-dark.png`、`tunnels-light.png`、`tunnel-status.png`。使用隔离配置和演示数据，不包含真实凭据。
- `cargo build --locked -p rshell` 通过，开发可执行文件位于 `target/debug/rshell.exe`。
- 2.3.9 打包前 `scripts/test.ps1 -Smoke` 通过：240 项测试、24 个窗口场景；日志为 `target/test-artifacts/package-2.3.9-check.log`。Release 构建通过，并使用 `target/release/rshell.exe` 完成隧道配置窗口验证（`release-2.3.9-tunnels.png`）。

## 待完成的验收

- 真实 Linux/OpenSSH 的转发许可策略、IPv4/IPv6 网络环境、断网恢复、长期运行及多会话压力验收。
- 2.3.9 按本地 Windows x64 轻量安装包和便携 ZIP 交付；未发布到线上。安装升级与真实用户操作验收尚未执行。
- 项目原有性能、真实输入法、全屏应用和安装验收继续保持开放。

## CLI：已实现（2026-11-06）

- 独立 `rshell-cli.exe` 控制台入口，支持 SSH `exec`、SFTP `ls/upload/download`，不创建 GUI、PTY 或交互 Shell。
- 已保存会话名称/UUID、显式主机/用户名/端口/私钥、标准输入密码/口令、读取现有加密凭据、配置目录优先级与整体超时。
- stdout/stderr 分离、远端退出码、JSON 目录列表、取消/错误退出码、流式 I/O；首次主机信任落盘后认证、密钥变化阻断、配置写锁冲突拒绝。
- 单文件上传下载沿用临时文件与同名目标不覆盖；不启用保存会话中的隧道或空闲发送。构建、ZIP 和 NSIS 脚本已加入 CLI；使用边界见 [CLI.md](CLI.md)。

## CLI：已验证

- 11 项 CLI 相关测试通过：4 项参数/密码输入测试、4 项核心回环测试、2 项真实 CLI 子进程集成测试，以及 1 项已保存 DPAPI 凭据读取专项。覆盖命令原样传递、中文输出与 stdout/stderr/退出码、密码与普通/加密私钥、错误认证、首次信任写锁与主机密钥变化阻断、已信任主机在写锁占用时只读连接、SFTP 二进制及中文空格路径往返、不覆盖已有文件、JSON 列表、缺失退出码、超时及连接释放。
- `scripts/test.ps1 -WithCredentials` 的格式、严格 Clippy 和全部 255 项常规测试通过（5 项专项默认忽略）；CLI 凭据与 DPAPI 专项随后通过，日志为 `target/test-artifacts/cli-workspace-recheck.log`。既有 JumpServer Windows 凭据测试在沙箱中写入失败，已在沙箱外补跑该测试及后续保存密码/MFA 测试，两项均通过，日志为 `target/test-artifacts/cli-windows-credentials-check.log`。未把沙箱内的整次脚本退出状态记为成功。
- 首轮检查遇到并行会话树开发的两个拖拽测试超时；最新复查已经通过。原始日志 `cli-workspace-check.log` 和中间过滤这两项的 `cli-regression-check.log` 保留作记录，后者不作为全量通过证据。
- 开发可执行文件为 `target/debug/rshell-cli.exe`；`--version` 和跨进程无窗口输出验证通过，三个修改过的 PowerShell 脚本语法检查通过。

## CLI：待完成与范围边界

- 真实 Linux/OpenSSH 服务上的 exec/SFTP、真实控制台 Ctrl+C、长时间/大文件/中断及并发验收尚未完成；本机 russh 回环测试不替代真实兼容性验收。
- 递归目录及同名内容更新已由后续 `sftp sync` 实现；单文件 upload/download 仍不覆盖。跨进程续传、权限/时间戳保留、SSH Agent、MFA、JumpServer 资产选择或命令 stdin 转发尚未实现。
- 本次未生成包含 CLI 的 release 安装包/ZIP，也未发布；既有 2.3.9 包不能视为已经包含 CLI，安装升级验收保持开放。没有 GUI 布局改动，本次未运行 `-Smoke`。

## SFTP 目录同步与 CLI 默认关闭：已实现（2026-11-06 后续）

- `sftp sync upload/download` 将源目录内容递归同步到目标根目录；支持空目录、完整 SHA-256 比较、跳过相同内容、新建及更新同名文件、`--dry-run` 和 JSON 操作清单。
- 默认保留目标多余条目；只有显式 `--delete` 才镜像删除。扫描及全部传输成功后重新检查源/目标，再逐项删除文件与空目录；不删除根目录，不执行递归删除。拒绝路径越界、符号链接/Windows 重解析点、特殊文件、大小写及文件/目录类型冲突；有条目、层级、路径和缓冲区上限。
- 上传覆盖要求服务端声明 OpenSSH posix-rename v1；先传临时文件、校验内容并复核原目标后发布。写入失败、源内容变化或取消不会进入后续删除阶段；同步不提供整棵目录事务或快照，已完成操作不会整体回滚。
- CLI 默认关闭，新配置和旧配置一致。通过“设置 → 会话 → 启用命令行（CLI）”后台保存开关；关闭时在凭据读取、密码 stdin 读取和 SSH 连接前拒绝新调用，帮助/版本查询保留可用。启用可使用当前用户已有会话凭据，不开放监听端口；关闭不终止已启动任务。开关不能隔离取得同一 Windows 用户权限的恶意程序，安全边界见 [CLI.md](CLI.md)。

## 目录同步与默认关闭：已验证

- 6 项同步核心回环测试通过：上传/下载内容更新、相同大小不同内容、中文/空格路径、嵌套与空目录、只预览不写入、默认保留与显式镜像、根目录/外部文件保护、缺少原子替换扩展、写入失败、路径冲突/链接/深度拒绝、传输中源变化、取消后的旧目标及额外文件保留。
- 实际 CLI 子进程验证同步方向、JSON 预览、默认保留和镜像删除；默认关闭/再次关闭拒绝执行与同步、旧配置默认值、GUI 开关点击保存的深浅主题事件测试通过。
- `scripts/test.ps1` 完整通过：格式、Clippy（警告视为错误）、当前工作区 268 项测试，0 失败、5 项专项默认忽略；日志 `target/test-artifacts/sftp-sync-and-cli-gate-final.log`。已启用配置的已有 DPAPI 凭据读取专项另行通过，日志 `target/test-artifacts/cli-gate-credentials-check.log`；未改变凭据存储后端。
- 原生设置窗口 800×520、180 px 窄侧栏下的深浅主题单场景检查通过；默认开关未勾选，文字与帮助图标可见。截图 `target/test-artifacts/cli-disabled-dark-narrow.png`、`cli-disabled-light-narrow.png`。采用隔离配置与演示数据，无真实凭据或服务器。

## 目录同步与默认关闭：待完成

- 真实 Linux/OpenSSH 目录同步、各种 SFTP 服务端扩展兼容性、大目录/长时间/网络中断、真实鼠标操作和并发文件修改验收；本机 russh 服务不代表这些验收通过。
- 不提供双向合并、持续监控、跨进程续传、权限/时间戳保留、全目录事务或安全隔离；未做独立安全审计或渗透测试。
- 开发版位于 `target/debug/rshell.exe` 和 `target/debug/rshell-cli.exe`；本次未重新打包或发布，运行了针对设置窗口的单场景检查，未运行整套 `-Smoke`。

## 会话树图标与拖动：已实现

- 使用统一 Lucide SVG 加载方式绘制 14 px 展开/收起文件夹图标，颜色随深浅主题调整；独立展开箭头、悬停拖动手柄及长名称省略显示，移除原文本拖动符号。
- 文件夹和会话整行拖动，支持同级混合排序、跨目录前后插入、移入文件夹、移回顶层；提供插入线、移入边框、列表边缘自动滚动和 Esc 取消。操作说明见 `docs/SESSION_TREE.md`。
- `session_order` 兼容旧配置，后台原子保存顺序和父目录；保护子树、稳定 ID 与凭据，拒绝循环嵌套、无效目标和同目录同名文件夹。

## 会话树：已验证

- 核心回归覆盖旧配置默认顺序、文件夹/会话混排、子树移动、相邻目标插入、配置保存重载、非法移动和无变化操作、凭据不变及写入失败回滚。
- egui 鼠标按下/移动/释放事件在深浅主题和 180 px 测试视口通过：文件夹及会话排序、混排、移入与移回顶层、搜索过滤后的定位、Esc 取消、循环移动拒绝，拖动未触发连接或登录编辑器。
- `scripts/test.ps1` 通过格式检查、严格 Clippy 与本轮工作区 255 项测试，0 失败、5 项专项测试默认忽略；日志 `target/test-artifacts/session-tree-check.log`。本次未变更凭据存储，未启用 `-WithCredentials`。
- 原生窗口深浅主题、常规宽度与 180 px 窄侧栏共 4 个场景截图已检查：`target/test-artifacts/session-tree-{dark,light,dark-narrow,light-narrow}.png`。采用隔离配置和本地演示数据，无真实服务器连接。
- `cargo build --locked -p rshell --bin rshell` 通过，开发版为 `target/debug/rshell.exe`。

## 会话树：待完成的验收

- 真实鼠标长列表拖动手感、多显示器不同 DPI 及用户配置规模下的性能验收。
- 本次未重新打包、安装或发布；已有 `dist/` 安装包不能作为包含本次改动的证明。

## 2026-11-12 默认 Shell 与断开回退（历史实现，提示符已由下述 Bash 替代）

- 已实现：启动页、新建标签和断开后按回车使用默认 Shell；默认是无需创建外部进程的 RShell 内置提示符，设置的“会话 → 默认 Shell”可切换本地 PowerShell，之后新建/回退时生效。
- 已实现：内置 `help`、`connect`、`reconnect`，连接仍通过现有认证与主机密钥流程；不执行操作系统命令。断开标签保留目标，回车后再输入 `connect` 可连接原目标。
- 已实现：原标签重连保留屏幕与有界滚动历史，旧连接后台输出与新连接终端隔离；主动启用关闭时清空或降低历史上限仍会丢弃相应记录。
- 已验证：`scripts/test.ps1`（fmt、Clippy、工作区测试）通过；内置提示符输入、回车进入、回环连接重连保留历史及旧输出隔离、两种默认 Shell 分支、深浅主题下设置点击与落盘已覆盖。实际窗口检查了深色 800×520 窄侧栏启动页和浅色设置页。
- 待验收：真实 Linux/OpenSSH 重连、真实 PowerShell 回退；未打包、未部署。首次全量检查中的下载取消清理既有测试失败，单项和后续全量复测通过。

## 2026-11-13 完整 Bash 语法

- 已实现：默认本地 Shell 改为 Git Bash，复用后台 PTY，支持 Bash 自身完整语法与 Git for Windows 提供的工具；设置中仍可选择 PowerShell。Git 不随本次构建打包，缺失时明确报错。
- 已实现：固定初始提示符检测、15 秒 Bash 启动超时、有界队列、会话内 1000 条历史且默认不写磁盘；不加载 `.bashrc` / profile，可由用户手动 source。Bash 是本机进程，不是 Linux 内核环境。
- 已实现：远端断开后回车进入本地 Shell，保留历史与原远端重连目标；本地 Bash 的 AI 执行身份明确标为本地 Bash，不沿用 PowerShell 标识。
- 已验证：`scripts/test.ps1` 通过格式、严格 Clippy 与工作区 273 项测试（6 项专项默认忽略）；另显式运行真实 Git Bash PTY 专项通过，覆盖数组、函数、循环、命令替换、管道、重定向、heredoc、中文、Tab 文件补全、上下键历史、Ctrl+C、退出与旧远端输出保留。日志为 `target/bash-final-checks.log`、`target/bash-pty-history-test.log`。
- 已验证：默认 Shell 设置在深浅主题中点击、保存及重载，Bash AI 提示身份与代码块匹配、切换 Shell 取消执行草稿；实际深色 800×520 窄侧栏与浅色设置页截图位于 `target/bash-gui/`。调试版 `target/debug/rshell.exe` 已构建。
- 待验收：真实 Linux/OpenSSH 重连、多显示器输入法、全屏应用及真实模型端到端；未打包发布。
