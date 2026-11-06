# Windows 本地测试指南

本地 PowerShell 的 `powershell_input_unicode_resize_and_exit` 回归会在测试子会话中先设置代码页 437，再显式设置 UTF-8 后输出中文；这用于消除系统区域设置对 `[Console]::WriteLine` 的影响，不修改系统全局编码。命令回显不能代替实际中文输出断言。可单独运行 `cargo test --locked -p rshell-core local::tests::powershell_input_unicode_resize_and_exit -- --exact`。

## 准备

1. 使用 Windows x64，安装 Rust 的 MSVC 工具链和 MSVC/Windows SDK。当前开发验证使用 Rust 1.98.1。
2. 打开 PowerShell，执行 `Set-Location D:\work\rshell`。
3. 首次构建需要访问 Cargo 配置中的依赖源。本项目不改写你的全局 Cargo 镜像或代理设置。
4. 无需 Node.js、浏览器运行时、数据库、Docker、WSL 或真实 SSH 服务器即可运行默认自动化测试。

## A. 仅看界面

JumpServer v4.10.2 集成的回环 API 测试可运行 `cargo test --locked -p rshell-core jumpserver`。该测试不连接生产服务器。`scripts/test.ps1 -WithCredentials` 额外使用随机目标名验证 Windows 凭据管理器的写入、读取和删除，并清理测试凭据。

针对配置窗口的单场景验证：使用隔离的 `RSHELL_CONFIG_DIR`，设置 `RSHELL_SCREENSHOT` 为 `target` 下的绝对 PNG 路径后运行 `target/debug/rshell.exe --demo --smoke-test --smoke-jumpserver`。只使用空白或虚构配置截图，不填写真实密码、验证码或令牌。该场景验证配置窗口，不代表真实登录或中转连接通过。

```powershell
.\scripts\dev.ps1 -Demo
```

预期：原生窗口打开，左侧会话列表，中部演示标签，终端显示中文和彩色文本。演示内容是固定测试数据，不能执行命令。可以检查字号、主题、选区、复制、查找、窗口缩放和配置界面。1.1.0 另需在会话设置中选中主机和用户名后按 Ctrl+C，粘贴到本机文本编辑器核对；对测试密码点击「复制」再粘贴核对，不要用真实凭据做截图或记录。

默认开发配置在 `target\local-config`，不会覆盖正常启动使用的默认用户配置。脚本退出时恢复调用前的 `RSHELL_CONFIG_DIR` 环境变量。

不用脚本时：

```powershell
$env:RSHELL_CONFIG_DIR = Join-Path $PWD 'target\local-config'
cargo run --locked -p rshell -- --demo
Remove-Item Env:RSHELL_CONFIG_DIR
```

最后一行只在你此前未设置该变量时使用；若已有值，应先保存原值再恢复。

终端查找栏单场景窗口验证：构建后，以隔离的 `RSHELL_CONFIG_DIR` 和 `target` 下的 `RSHELL_SCREENSHOT` 运行 `target/debug/rshell.exe --demo --smoke-test --smoke-terminal-search`，自动打开 800×520 窗口及右上角查找栏，检查小字号与窄窗口布局后自动退出。

退出确认框单场景窗口验证：使用隔离配置和 `target` 下的截图路径，运行 `target/debug/rshell.exe --demo --smoke-test --smoke-confirm-exit`，在 800×520 窗口内检查弹窗留白、提示文字和右对齐按钮。隔离配置中的 `light_theme` 分别设为 `false` / `true`，检查深浅主题；加 `--smoke-tree-narrow` 检查 180 px 窄侧栏。场景自动打开弹窗并退出，不连接服务器，不代表真实会话断开验收。

## B. 本机 SSH 手动连接

SSH 隧道自动化验证使用 `cargo test --locked -p rshell-core --test tunnels`。仅使用回环地址上的 russh 和 TCP 测试服务，不连接生产服务器。测试服务不提供 Shell，以验证仅隧道模式。

隧道配置单场景窗口验证：构建后使用隔离的 `RSHELL_CONFIG_DIR`，设置 `RSHELL_SCREENSHOT` 为 `target/test-artifacts` 内绝对路径，运行 `target/debug/rshell.exe --demo --smoke-test --smoke-tunnels`；加 `--smoke-light` 检查浅色主题。`--smoke-tunnel-status` 场景显示 800×520 窗口中的隧道状态，仅使用虚构数据，不建立网络监听。测试运行后自动退出。

`scripts/test.ps1 -Smoke` 包含上述三个隧道窗口场景，连同其他界面共运行 24 个窗口场景；打包前执行整套检查。

终端窗口 1：

```powershell
cargo run --locked -p rshell-core --example local_ssh_server
```

终端窗口 2：

```powershell
.\scripts\dev.ps1
```

在客户端创建 `127.0.0.1:2222`、用户名 `demo`、密码 `rshell-demo` 的会话。记录服务启动时打印的 SHA-256 指纹，连接后可在设置中与自动保存的记录对照。

测试顺序：

1. 首次连接不出现指纹确认弹窗；自动保存主机密钥后看到本机测试服务欢迎文本。
2. 在「设置 → 已信任的 SSH 主机」检查记录的指纹与测试服务输出一致；再次连接仍可进入终端。
3. 输入 `help` 后 Enter；输入中文，确认回显不乱码。
4. 输入 `colors` 检查 ANSI 颜色；缩放窗口，确认终端尺寸通知。
5. 拖动选择文字，点击「复制选区」，在你自己的文本编辑器中检查复制结果。
6. 粘贴两行测试文本，先取消再确认，检查多行粘贴弹窗。
7. 从同一配置再打开一个连接，检查标签输入互不影响。
8. 输入 `exit` 让服务关闭该 channel，确认原标签仍显示该会话名和断开状态，点击顶部工具栏的“重新连接”，核对原标签重新连接到原目标，标签数量保持一致，再测试主动断开。
9. 保存会话、修改字号或主题，退出客户端并重新启动，检查持久化。

测试服务只处理 `help`、`colors`、`clear`、`exit` 及文本回显，不调用系统 shell；**不能用它验收 bash/vim/top/less/tmux**。

默认主机密钥保存在 `target/local-ssh/host_ed25519`，重启复用，所以指纹保持一致。如果你主动删除或替换该测试密钥，客户端应提示主机密钥变化。不要将这种提示当作故障而盲目绕过；先核对测试服务与配置记录。

端口被占用时可指定另一个端口：

```powershell
cargo run --locked -p rshell-core --example local_ssh_server -- 2223
```

客户端同时改用 `2223`。服务端按 Ctrl+C 退出，不会停止其他 SSH 服务。

## C. 自动化测试

```powershell
.\scripts\test.ps1
```

脚本遇到任何命令失败立即停止，不会让后续成功命令掩盖前面的失败。

| 命令 | 覆盖内容 |
| --- | --- |
| `cargo fmt --all -- --check` | 格式一致性 |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | 含示例与测试的严格静态检查 |
| `cargo test --locked --workspace --all-targets` | GUI 事件、配置与终端模型、SSH 集成测试 |
| `cargo test --locked -p rshell-core --test ssh_loopback` | 单独运行 SSH 协议测试 |
| `cargo test --locked -p rshell-core --test headless` | 无界面 exec/SFTP、主机信任与密钥认证回环测试 |
| `cargo test --locked -p rshell-core --test sftp_sync` | 目录递归/预览/镜像、内容变化、写入失败与路径冲突保护 |
| `cargo test --locked -p rshell --bin rshell-cli --test cli` | CLI 参数、实际子进程输出/退出码、SFTP 与超时回环测试 |
| `cargo test --locked -p rshell --bin rshell` | GUI 键盘/IME 事件、粘贴与弹窗隔离 |
| `cargo test --locked -p rshell-terminal` | UTF-8 分片、ANSI、备用屏幕、查询回复、历史搜索 |

GUI 测试使用框架内事件输入，不能替代实际 Windows 输入法候选框和物理键盘验收。SSH 自动化测试使用随机本地端口和临时主机密钥，与上面手动启动的 2222 服务相互独立。

## D. RShell 加密凭据与真实窗口

```powershell
.\scripts\test.ps1 -WithCredentials -Smoke
```

- 凭据测试在 `target/credential-tests/` 下创建独立临时目录，验证加密读写、覆盖、空口令、错误 UUID 拒绝、损坏文件拒绝和删除；不访问凭据管理器或已有会话密码。
- 冒烟测试使用单独配置目录，启动演示窗口，约 6 秒后退出。
- 截图输出：`target/test-artifacts/smoke-demo.png` 和 `smoke-local.png`。它由本程序的渲染器产生，不截取其他应用或桌面内容。
- 该检查需要可运行原生图形窗口的 Windows 用户会话，不能等同于无桌面 CI 环境。

## E. 真实 Linux/OpenSSH 兼容性测试

会话文件手工验收：确认「文件 → 导入/导出」中的「导出全部 RShell 会话 (.rsh)…」与「导出全部给 Xshell (.xts)…」直接打开保存对话框，不再显示会话选择子菜单。RShell 一次导出全部已保存会话（含串口）；Xshell 一次导出全部 SSH/Telnet 会话并跳过串口，没有兼容会话时禁用入口。左侧会话右键导出也按相同规则导出全部会话。检查目标文件存在；从同一子菜单导入 `.rsh`，确认全部生成新会话且原会话不变，旧版单会话 `.rsh` 仍可导入。导出保留公开连接设置，不包含目录分组；Xshell 格式的重名会话自动添加编号避免覆盖。使用 Xshell 客户端导入 `.xts`，核对会话数量、名称、主机、端口和用户名。两种文件都不应出现密码、私钥内容或私钥路径；凭据和主机信任需重新配置。全部导出的本地往返测试与 GUI 菜单检查不代表真实 Xshell 客户端兼容性验收通过。

自行提供有权限访问的测试服务器，在 GUI 中填写连接信息。不要把生产密码或私钥提交到仓库。建议先确认主机指纹，再按顺序测试：

| 场景 | 操作与预期 |
| --- | --- |
| bash | 英文/中文输入、方向键历史、Tab 补全、Ctrl+C 中断、Ctrl+D 退出 |
| vim | 编辑文本、移动光标、中文宽字符、切换模式、缩放窗口、退出后恢复屏幕 |
| top/less | 刷新、滚动、搜索和退出后恢复 shell |
| tmux | 窗格/状态栏/鼠标交互、调整尺寸及退出 |
| 私钥 | 普通及加密私钥成功；错误口令失败后可重新输入 |
| 网络 | 断网能发现错误；网络恢复后手动重连，不自动重放命令 |
| 多连接 | 多标签各自输入、关闭、重连，互不影响 |

10 分钟输出压力、8 小时长连接、P95 延迟、内存和句柄趋势仍按原方案验收。短时间单元测试通过不代表这些指标达标。

## F. 常见问题

| 现象 | 处理 |
| --- | --- |
| `cargo` 找不到 | 安装 Rust 后重新打开 PowerShell，检查 PATH |
| `link.exe` 或 Windows SDK 缺失 | 按 Rust 官方 MSVC 指南安装 C++ 构建工具和 SDK |
| 依赖下载失败 | 检查本机 Cargo 源、代理与网络；保留完整错误，不随意关闭 TLS 校验 |
| 脚本被执行策略阻止 | 改用 README 中的直接 Cargo 命令，无需更改全局执行策略 |
| 程序提示已在运行 | 关闭使用同一配置目录的实例，或指定新的开发配置目录；不要删除一个活跃实例的锁文件 |
| 无法覆盖 `rshell.exe` | 先关闭之前启动的程序，再重新构建 |
| 配置损坏 | 先备份损坏文件；检查 `config.json.bak`，确认内容后在应用退出时恢复 |
| 主机指纹变化 | 先通过可信渠道核实；当前独立更新界面未完成，不在生产环境试错 |
| 中文显示缺字 | 应用读取 Windows 微软雅黑/黑体/宋体回退；记录系统字体、DPI 与截图 |
| 图形初始化失败 | 记录 GPU/驱动及错误文本；目前使用 wgpu 后端，兼容性尚需扩展验证 |

## 测试反馈

请记录：命令、Rust/Windows 版本、认证类型（不含秘密）、预期、实际结果、能否复现。命令输出可保存到 `target/test-artifacts/`，截图前清除密码、密钥及终端中的业务敏感信息。


## 默认本地终端与连接窗口

正常启动（不加 `--demo`）后，主区域应显示 PowerShell 提示符，可输入 `Get-Date` 和 `ssh -V`。本机 ConPTY 自动化测试需要系统已安装 OpenSSH 客户端。连接测试服务可直接输入 `ssh -p 2222 demo@127.0.0.1`，或者使用「新建会话」。两种入口各自维护主机信任记录。

从左侧打开一个测试会话并完成连接操作，确认启动时自动创建的 PowerShell 标签关闭，连接标签保持选中；打开认证窗口后取消时，PowerShell 应保留。手动再开一个本地 PowerShell 后连接其他会话，确认手动标签保留。此检查使用本机回环测试服务，不连接生产服务器。

手工检查：新建会话→私钥→浏览，选中文件后应填入路径；取消文件选择不应覆盖原路径。密码默认遮挡，显示按钮可切换，在密码框按 Enter 应仅连接。关闭旧编辑窗口后文件选择结果不得写入另一个会话。真实文件对话框点击、密码操作和远程登录仍需人工验收。

`-Smoke` 分别启动演示窗口与默认本地终端，保存 `target/test-artifacts/smoke-demo.png`、`smoke-local.png`，并检查 30 秒内退出。测试截图不包含真实连接凭据。

## 主机密钥核验与更新

用本机测试服务建立一次内置 SSH 会话后，检查设置中自动保存的主机记录应包含算法、公钥和记录时间。打开核验窗口，输入错误指纹必须保留旧记录并给出错误；输入与公钥一致且经独立核验的指纹后可更新。更新仅对新连接生效，不重新放行先前被阻断的连接。

自动化测试覆盖 schema 2 旧主机迁移、密钥/指纹不一致拒绝、密钥轮换撤销旧信任、过期操作拒绝、写入失败保留原文件和取消确认不持久化。实际设置窗口的点击、滚动和复制粘贴仍需人工验收。

当 release exe 正在使用、构建报拒绝访问时，不必停止已有会话。执行 `.\scripts\build.ps1 -Package -TargetDirectory target/windows-next` 在独立目录构建新版；正常退出旧窗口后，再从新路径启动。默认配置目录有单实例保护，同时测试不同版本时应使用不同的 `RSHELL_CONFIG_DIR`。

## 经典布局检查

使用新开发配置检查浅灰菜单、线条工具图标、地址栏、会话快捷栏、白色侧栏、蓝色标签及黑色终端。已有深色配置通过「选项 → 设置 → 浅色界面」切换。人工操作时检查各菜单、工具栏提示、侧栏隐藏/恢复、地址栏输入主机或已保存名称后打开认证窗口、标签关闭及新增。保存两个会话后，检查快捷栏初始为空、点击「+」或侧栏右键添加、重复添加不出现副本、右键移除和删除会话后清理快捷项，并重启检查持久化。添加足够多的快捷项使其溢出，检查没有黑色横向滚动条、「+」始终可见、左右按钮能翻到首尾、窗口缩放后仍可操作快捷项和拖入会话；未完成的人工检查不要以窗口冒烟代替。

## 标签菜单回归

标签栏回归：`scripts/test.ps1 -Smoke` 会生成 `smoke-tabs-fit.png` 和 `smoke-tabs-overflow.png`。前者检查单标签不超过 140 逻辑像素且标签栏高度收紧，后者检查标签溢出时选中的末尾标签自动出现在可视范围。人工打开多个标签并改变窗口宽度，核对高度、标题省略、关闭按钮的点击区域和完整名称悬停提示；鼠标放在标签标题区域滚动滚轮，确认标签栏向左右滑动、到两端停止且当前标签不变，移到终端区域再滚动应只回看终端历史；点击「◀」「▶」，按 Ctrl+PageUp / Ctrl+PageDown 及 Ctrl+Alt+← / Ctrl+Alt+→，检查相邻标签切换和首尾循环；只按方向键时仍应发送终端输入。打开设置或重命名弹窗时，快捷键不应切换底层标签。

运行 `scripts/test.ps1 -Smoke`。标签逻辑单元测试覆盖关闭后台标签后保留当前选中身份、右侧/本地/其他/全部关闭。回环 SSH 测试 `duplicate_channels_share_authentication_and_close_independently` 验证一次认证复用多个渠道、输出隔离、关闭原标签与复制标签后其他渠道继续输入，以及最后渠道关闭释放连接。

人工操作：打开多个本地和内置 SSH 标签，右键非当前标签测试各关闭范围；让一个远端标签正常断开、另一个连接失败，检查「关闭断开的连接」只关闭这两个标签，保留正在连接、已连接和本地标签，没有断开标签时菜单项禁用；重命名后检查保存会话名称不变；测试 Shift+Alt+N、Ctrl+Shift+F4，并在重命名窗口内输入确认内容不发送到终端。复制会话需新认证，复制 SSH 渠道应直接打开新 shell。真实 Linux/OpenSSH、多渠道数量限制与拒绝响应仍需目标环境验收。

终端交互验收：输出超过一屏后拖动右侧竖向滚动条，检查可回看、回到底部且不会把拖动当成文本选区；右键非当前标签选择「清屏」，确认只清除该标签的显示和历史，连接仍可继续输出。在终端内容区右键选择「粘贴」，分别粘贴单行和多行文本，确认与 Ctrl+V 使用同一过滤规则及多行确认；启用鼠标报告的远端全屏程序中确认右键仍传给程序。


## AI 配置验证

执行 `scripts/test.ps1 -Smoke -WithCredentials`。AI 核心测试覆盖地址与必填校验、旧配置兼容、新增/编辑/默认/删除、密钥保留和清除、保存及删除失败补偿。Windows 凭据专项覆盖现有 DPAPI 后端；不调用真实 AI 服务。

人工检查入口为「设置 → AI」。新增配置后重启检查持久化，编辑时密钥留空应保留，取消加密保存后密钥应删除。`--ai-settings --demo --smoke-test` 可生成该页面渲染截图；自动截图不等同于完整人工交互验收。


AI 右侧面板：运行 `scripts/test.ps1 -Smoke` 检查默认窗口渲染。人工验收项包括拖动分隔线、收起/重开、配置入口、草稿输入与终端焦点切换；发送按钮应禁用，输入草稿不能发送到终端。草稿不写入配置文件。

## 保持活动状态验证

本次新增核心测试覆盖旧会话默认值、序列化及非法间隔/字符串拒绝；回环 SSH 测试覆盖持续流量推迟空闲发送、UTF-8 原样发送、禁用不发送及断开后停止。执行 `scripts/test.ps1 -Smoke`。

人工检查：打开「设置 → 连接 · 保持活动」，选择已保存的 SSH 会话，切换三项开关，确认间隔/字符串控件跟随启用状态；点击「保存保活设置」并重启确认值保留。使用测试 SSH 主机分别验证 SSH 消息、空闲字符串与 TCP 保活。TCP 使用系统默认探测时间，不能以一两分钟未断开判定其已通过。Linux/OpenSSH 兼容性、断网探测和 8 小时长连接验收仍开放。

## 串口验证

运行 `scripts/test.ps1 -Smoke` 检查整体回归和 demo/local/serial 三种原生窗口；serial 场景使用 `--demo --smoke-test --smoke-serial` 打开串口设置页但不连接设备，截图保存在 `target/test-artifacts/smoke-serial.png`。串口自动测试覆盖旧 SSH 配置默认值、串口配置保存重载、非法参数、模拟字节输入/输出与终端回复、尺寸、读超时、写失败和取消；模拟 I/O 不等于真实驱动或硬件通过。

手动验收使用专用 USB 串口回环设备或虚拟 COM 端口对：
1. 新建会话选择串口，刷新端口并填写与对端一致的参数，保存后重启确认参数保留。
2. 双向发送 ASCII、分片 UTF-8 和 ANSI 控制序列，核对终端显示及发送字节。测试常用波特率、校验与流控制，具体组合以硬件支持为准。
3. 验证关闭后端口可重新打开、占用端口报错、拔出设备报错，以及流控阻塞/大段粘贴时关闭标签能结束工作线程。
4. 确认串口会话不出现密码、主机确认和 SSH 保活操作；本地 PowerShell 和 SSH 原回归保持通过。

未接入实际串口硬件，以上人工验收保持待完成。

## 左右设置布局

`scripts/test.ps1 -Smoke` 额外运行 AI 设置窗口截图（smoke-ai-settings.png），串口场景覆盖会话左侧协议导航。人工检查各设置分类切换、SSH / 串口切换、缩放与滚动、保存/取消及保活选择不同会话。保活后台事务测试覆盖保存重载、凭据及其他字段保留、非法参数与不存在/非 SSH 会话拒绝。

AI 面板默认关闭：点击工具栏「AI 对话」打开。检查 Enter 发送、Shift+Enter 换行、失败保留草稿；新对话后从顶部下拉框或历史打开旧会话，重启后仍可读取，删除后不可恢复。连接下拉只列已连接的本地 PowerShell/SSH 标签；在终端选中文字，右键选择「添加选中内容到 AI 对话」，确认内容进入可编辑草稿、目标连接选中且不会自动发送；长内容应只在输入框内部滚动，发送按钮始终可见；超长选区会提示截取。用回环模型返回 `bash`/`powershell` 代码块，检查文本选择、复制、执行确认、目标断开和黑名单阻断。`scripts/test.ps1 -Smoke` 截图默认关闭与显式展开状态（ai-chat），以及长草稿布局（ai-chat-long）。真实服务、鼠标和输入法交互仍须人工验收。

间距调整增加两种冒烟入口（需配合 --smoke-test）：--smoke-appearance 打开外观设置，--smoke-session 打开空白 SSH 会话。scripts/test.ps1 -Smoke 自动执行并生成 smoke-appearance.png、smoke-session.png；检查导航、表单及底部按钮完整。

脚本支持 CARGO_TARGET_DIR 指定构建目录；运行中的默认 debug 程序被占用时，可设置为 target/layout-check 再执行 -Smoke。截图仍输出到 target/test-artifacts。

终端缩放回归：分别在本地 PowerShell 和内置 SSH 标签中用 Ctrl + 滚轮调整字号，同时按住左键也应生效；普通滚轮仍滚动历史。确认其他标签、全局界面比例不变，弹窗打开时不缩放，尺寸变化发送 PTY/ConPTY Resize，缩放滚轮不发送终端输入。自动化用例筛选：cargo test --locked -p rshell wheel。真实鼠标手势和远端全屏程序仍需人工验收。

## 文件传输验证

`cargo test --locked -p rshell-core --test ssh_loopback` 包括回环 SSH 的 ZMODEM 双向二进制与 shell 恢复、SFTP 浏览/上传/下载/同名保护/取消/复用认证。目录和文件数据完全为测试生成，不连接生产服务器。`cargo test --locked -p rshell-core transfer` 覆盖文件名防护和本地发布不覆盖；`cargo test --locked -p rshell-core zmodem` 覆盖握手分片和 ZMODEM 集成测试。

`scripts/test.ps1 -Smoke` 新增 sftp 场景；`--demo --smoke-test --smoke-sftp` 只打开演示窗口，不连接远端。完整脚本若被其他工作区告警阻断，应分别记录阻断和单独测试结果，不能视作整体通过。

真实测试主机人工验收（当前未完成）：
1. 在专用测试 SSH 会话中执行 `sz` 发送空文件、中文名、二进制、多文件；逐一比较 SHA-256，测试取消和同名文件保护，确认传输后 shell 继续可用。
2. 执行 `rz`，选择本地空文件、中文名、二进制文件，比较 SHA-256；取消文件选择和传输中按 Esc/Ctrl+C，检查远端退出传输并返回 shell。远端 rz 的覆盖选项需由操作者自行确认。
3. 打开 SFTP，验证初始目录、相对/绝对路径、上级、刷新、无权限目录、服务未开启 SFTP 时的错误。上传/下载并核对哈希；同时使用终端，确认无第二次认证。
4. 传输大文件时观察内存、字节进度；取消、关闭窗口、关闭标签、断网后检查未完成文件以 `.rshell-UUID.part` 保留，原有同名文件未改变。大目录超过 10000 项应报错而非无限占用内存。
5. 本地 NTFS 上检查最终发布和同名目标保护；其他文件系统需验证硬链接支持。不支持时应保留完整临时文件并报告错误，不得显示成功。

当前 ZMODEM 只为内置 SSH 接入，单个上传文件小于 4 GiB；非 UTF-8 文件名不支持。不以自动测试代替实际 Linux/OpenSSH/lrzsz、原生对话框和长时间传输验收。


### 选区、光标与剪贴板设置（2026-10-27）

- `scripts/test.ps1 -Smoke` 包含 GUI 拖选左右方向、释放时复制及关闭开关、粘贴尾部清理及关闭开关、旧配置默认开启和后台保存重载测试。
- 人工复核：在本地测试文本（含中文、空格、换行）上正向/反向拖选，检查高亮与文字对齐；在不同 DPI、字号及历史回看中重复。确认光标为实心且覆盖字符可读。
- 设置中分别关闭两个开关，重启后确认保留。复制关闭时剪贴板应保持原值，手动复制仍可用；清理关闭时粘贴尾部空白保留，有换行需确认。
- 粘贴 `  echo hello` 后附空格、Tab、CRLF：默认保留前导空格、去掉尾部空白，不自动执行；中间含换行仍需确认。请使用本机隔离终端，不连接生产主机。
- 框架事件测试及窗口截图不替代真实鼠标、多显示器 DPI、IME、Linux/OpenSSH 和全屏应用验收。


### 会话管理器缩放（2026-10-27）

搜索框下方不再显示缩放按钮和比例；用下述 Ctrl + 滚轮手势检查缩小、放大和 75%–175% 边界，并观察标题、搜索框、文件夹和会话行是否同步变化。

拖放验收：在隔离配置中创建两个文件夹和一个会话，把会话拖入另一文件夹、拖回「所有会话」；用文件夹右侧「⋮⋮」拖动文件夹，验证原目录下的会话随之移动。把父文件夹拖向子文件夹应无法投放；重启后检查目录归属仍在。把会话拖到顶部快捷连接栏，确认只增加一个入口，点击可打开，重复拖入不重复；搜索后用「×」清空。右键移动和编辑器文件夹选择仍应可用。窗口冒烟截图只验证绘制，不代替这些鼠标操作。

1. 打开带多级文件夹和会话的窗口，将鼠标放在左侧会话管理器，按住 Ctrl 滚动滚轮，不按鼠标键。确认标题、搜索框、文件夹和会话行一起放大或缩小，树形缩进与点击区域同步变化。
2. 普通滚轮不缩放，仍用于列表滚动。把鼠标移到终端后按 Ctrl 滚轮，确认只作用于终端缩放，不改变侧栏比例。
3. 缩放到 75% 与 175% 边界，在浅色和深色主题下检查文字、图标不重叠；弹出设置或会话编辑窗口时，底层侧栏不响应缩放。调整到 130%，关闭并重新启动，确认恢复上次比例；旧配置缺少此字段时默认 100%。
4. `scripts/test.ps1 -Smoke` 检查构建、已有窗口冒烟及滚轮修饰键规则。真实鼠标手势与不同 DPI 显示需人工验收。

最近连接入口：确认左侧会话管理器不再显示最近连接；打开「文件 → 最近连接」，空记录时显示「暂无最近连接」，有记录时按最近使用顺序显示已保存会话；选择一项后仍按原认证与主机密钥校验流程连接。

### XTS 导入与 Telnet（2026-10-27）

中文路径回归：使用 GBK 原始文件名、未设置 ZIP UTF-8 标记且没有 Unicode 扩展字段的归档，验证中文文件夹、空目录与会话名；不能只用 ZipWriter 默认生成的 UTF-8 文件名。已有乱码通过原 XTS 的「修复已导入名称」入口修复，核验 UUID/用户名/认证设置/凭据不变、重复修复为 0、手动改名不覆盖、名称冲突与保存失败不发布配置。真实样本测试额外断言「C端」「交换机」「开发环境」「线上服务器」等中文名称，并对比旧解析结果修复前后；原数量/重载测试不能单独证明中文解码正确。

1. 使用 `scripts/dev.ps1 -Demo` 隔离启动，从「文件 → 导入/导出 → 导入 Xshell 会话 (.xts)…」选择文件或输入路径。点击导入后检查分类数量、文件夹层级、空目录、SSH/Telnet 主机端口和用户名。重复导入应新建编号根目录，原会话 ID 与凭据不变。
2. 密码、私钥、脚本与主机信任不迁移；缺用户名的 SSH 在连接前补填。串口枚举尚未兼容，跳过计数应明确显示。损坏或超限包失败时不修改已有配置；无效单会话跳过并计数。
3. 新建 Telnet 会话，检查默认端口 23、明文提示和没有密码保存控件。Telnet 账号密码在远端终端提示后手动输入；本机测试仅使用回环服务验证 IAC 分片、回显协商、UTF-8 数据、窗口尺寸、输入与主动断开，不连接生产服务器。
4. `scripts/test.ps1 -Smoke` 增加 `smoke-import.png` 和 `smoke-telnet.png`。真实选择器点击、网络设备 Telnet 兼容性仍需人工验收；不以本机协议测试替代真实设备验收。
5. 实际导出文件的显式验证命令如下；只解析、保存并重新加载配置，不发起连接，不打印密码或会话明细。输出位于 `target/xts-validation/config.json`，属于含主机/用户名的本机开发数据，不应提交到仓库。

```powershell
$env:RSHELL_TEST_XTS = 'C:\Users\Administrator\Desktop\xbackup20261027.xts'
cargo test --locked -p rshell-core actual_export_roundtrip -- --ignored --nocapture
Remove-Item Env:RSHELL_TEST_XTS
```

### 会话选项（2026-10-27）

打开「选项 → 设置 → 会话」，逐项切换并重启，确认设置保留。建立测试用远端连接，分别检查关闭标签、工具栏断开和窗口退出的确认与取消；断开后检查同标签 PowerShell 是否启动，开关关闭时是否保留远端结束状态。先产生超过一屏输出，分别检查清除回看开关的两种结果。编辑已保存会话后点击「连接（自动保存）」应保存非凭据修改；关闭自动保存时「仅连接」不应保存，改变登录目标或凭据必须显式保存。此检查不连接生产服务器。

`scripts/test.ps1 -Smoke` 包含会话设置页截图 `target/test-artifacts/smoke-session-options.png`，只验证原生窗口布局，不代替上述手工行为检查。



### Geist 字体检查（2026-10-27）

- 打开 demo 和本地 PowerShell，检查英文命令、数字列、标点、中文、彩色 ANSI、光标与拖选高亮。终端网格应保持固定列宽，中文由 Windows 字体回退；Ctrl + 滚轮改变字号后再次检查对齐。
- 浅色和深色界面的菜单、会话树、设置及弹窗使用 Geist Variable；检查中英文混排与截断。截图只能证明渲染，真实 DPI 和 IME 仍需人工检查。
- `scripts/build.ps1 -Package` 生成的 ZIP 应包含 `licenses/Geist-OFL.txt`，字体本体已嵌入 exe。

AI 回复样式验收：检查浅色与深色主题中的正文、列表、强调文字、行内代码和代码卡片；复制代码和整条回复应保持原始文本。将长目录列表加入草稿后，确认仅输入区域内部滚动，模式、模型和发送按钮始终可见；缩窄面板后标题不挤占工具按钮。对话区和输入区滚动条默认隐藏，鼠标移入有溢出内容的区域时显示，移出后淡出。

AI 工具栏验收：当前对话导出为 Markdown 后核对中文、多行和代码块；取消保存应不生成文件，写入失败应显示错误。新对话保留历史，清空只移除当前对话及草稿。最大化与还原切换后，确认连接保持、对话/草稿保留且未把点击发送到终端。调整 AI 字体与字号、重启后检查保持，一键重置恢复跟随界面字体及字号 12。窗口 Smoke 包含 ai-chat-maximized 场景。
AI 请求与大字号回归：发送后在等待期间输入另一段草稿，制造请求失败或点击停止，检查新草稿未被覆盖；空输入框应恢复原问题。AI 字号设为 24 时，正文/代码/输入内容变大，但工具栏、模型/连接选项与发送按钮不放大、不越出面板。Smoke 增加 ai-chat-font-large 场景。程序内事件测试覆盖新对话、清空、最大化/还原；回环模型测试贯通读取加密密钥、请求、历史保存、点击代码执行、编辑和确认，以及切换目标取消。模拟终端收集命令用于断言，不连接真实服务器。

### JumpServer 自动刷新间隔（2026-10-28）

授权同步格式回归：使用同时包含 `favorite` / `ungrouped` 虚拟节点和资产 `nodes: [{id, name}]` 的回环响应，验证分页完成、真实节点与资产归属关联以及后续账号查询和令牌申请。另验证对象 ID 缺失、非法 UUID、错误类型被拒绝，UUID 字符串缓存仍可重载。

已实现：配置窗口可设置 1–43200 分钟，默认 60 分钟；旧配置缺失字段时采用 60 分钟，保存后重新计时。
已验证：scripts/test.ps1 通过，覆盖默认值、旧配置加载、自定义间隔序列化及非法范围；单场景窗口截图 target/test-artifacts/jump-interval.png 显示默认 60 分钟且无截断。
调度回归：查询账号、发送验证码和连接请求等非同步结果不得重置刷新计时；自动测试注入非同步结果并核对原到期时间保持不变。
待验收：真实 JumpServer 登录后跨刷新周期的端到端验证。

### SSH 密钥管理器

`-Smoke` 新增 `host-keys` 和 `user-keys` 两个窗口场景；截图位于 `target/test-artifacts/smoke-host-keys.png` 与 `smoke-user-keys.png`。手工验收：从工具菜单打开窗口，生成 RSA/Ed25519、导入现有私钥、查看属性、导出并在其他环境导入、将密钥关联到会话再保存、删除前取消/确认。主机密钥更新须从独立可信渠道核验公钥及指纹。窗口检查不代表真实 OpenSSH 或完整文件选择器交互通过。

### 独立分屏标签与分隔条

`--demo --smoke-test --smoke-split` 显示四屏、每屏两个独立标签。GUI 事件回归覆盖左右/上下/网格拖动调整尺寸、输入隔离、屏内切换、跨屏拖动，以及关闭/新增的分组状态。手工检查：拖动分隔线至边缘、逐屏切换标签、拖至空屏、点击各屏 +、在当前屏打开 SSH、关闭最后一个标签、切换单终端与网格；真实 SSH、IME、全屏程序和不同 DPI 仍需独立验收。

### 界面隐藏恢复与回看上限

完整 Smoke 新增 `hidden-chrome`（顶部四栏全部隐藏）和 `history-settings`（设置中的回看编辑区），共 21 个窗口场景。GUI 事件测试从四栏隐藏状态右击底部状态栏，再点击「显示全部栏」，核对菜单恢复；快捷键测试确认 Ctrl+Shift+M 被界面消费、不传入终端。

回看测试覆盖旧配置默认值、保存重载、非法上限拒绝、已有/新建终端应用、新旧输出保留、备用屏幕下修改上限以及 0 行关闭回看。手工补验不同 DPI、较大历史量与真实全屏程序。


## 内置 Agent SDK（2026-10-29）

新增回环配置探针：`cargo run --locked -p rshell-core --example claude_agent_probe -- --loopback-profile`。它使用真实冻结 SDK，但仅访问本机模拟 Anthropic API，断言所选配置的路径、合成 API Key 和自定义模型实际出现在请求中；不读取现有供应商密钥，不产生真实模型用量，不执行终端命令。真实供应商兼容性仍须另验收。

当前默认是 `Agent · 通用问答`。定向窗口参数：`--demo --smoke-test --smoke-ai-chat` 检查默认布局，追加 `--smoke-agent-menu` 检查 Ask/Agent 和三项模式菜单，追加 `--smoke-agent-execute` 检查执行模式。GUI 回归覆盖默认非执行、显式选择执行、切回 Ask、同名模型配置按 ID 切换以及凭据读取期间切换目标取消。`--smoke-agent` 旧参数可保留，默认界面已经是 Agent。

模型设置新增 OpenAI/Anthropic 接口类型；旧配置保持 OpenAI。通用问答与命令建议测试分别核对 `/chat/completions` 与 `/v1/messages`，执行模式要求 Anthropic。配置密钥仍由现有后台 DPAPI 存储读取。凭据专项若被沙箱限制 Windows 凭据管理器写入，应单独记录沙箱外 `jumpserver_credentials_roundtrip -- --ignored` 的结果，不将沙箱失败隐藏为整套成功。

默认 `scripts/test.ps1` 不调用真实模型。Agent 的 GUI 回归覆盖命令确认前不发送、编辑后精确发送一次、等待用户提交输出、切换/断开目标及工具超时撤销。核心测试覆盖回环桥接认证、人工响应前阻塞、工具白名单、敏感行遮盖、SDK 请求/旧配置迁移，以及 Windows Job Object 对父子进程的清理。

单场景窗口验证：构建后设置隔离的 `RSHELL_CONFIG_DIR` 和位于 `target/test-artifacts` 下的 `RSHELL_SCREENSHOT`，运行 `target/debug/rshell.exe --demo --smoke-test --smoke-ai-chat --smoke-agent`。另用 `--demo --smoke-test --ai-settings` 查看 Agent 设置。截图不得填写真实密钥。

先构建内置组件：`scripts/build-agent-sdk.ps1 -InstallDependencies`。需要 Python 3.10+ x64，仅安装依赖到 `target/agent-sdk-venv`；构建会运行 2 项桥接边界测试、冻结组件及离线引擎自检，输出到 `target/debug/agent-sdk`。最终用户不需要 Python 或 CLI。`agent-sdk/requirements.txt` 固定构建依赖；组件附带版本及 SHA-256 清单。

可选真实 SDK/MCP 探针（使用 API 认证，产生模型用量）：

```powershell
$previousAgentProbe = $env:RSHELL_CLAUDE_LIVE_TEST
try {
    $env:RSHELL_CLAUDE_LIVE_TEST = '1'
    cargo run --locked -p rshell-core --example claude_agent_probe
} finally {
    $env:RSHELL_CLAUDE_LIVE_TEST = $previousAgentProbe
}
```

探针只提供虚构 PowerShell 视口，要求读取一次并提出 `Get-Date`；宿主固定拒绝命令，不连接任何真实终端。成功应输出 `LIVE PROBE PASSED: reads=1, proposals=1, executed=0`。SDK 0.2.174 的冻结组件及配套引擎 2.1.310 已通过该探针，认证采用用户 settings 的白名单 API 环境项，不使用订阅登录状态。该结果不代表真实 SSH Shell 或其他认证方式通过。

真实操作验收：使用测试用本地 PowerShell/SSH，会话空提示符下提出只读命令，检查目标与用途，确认发送一次，完成后刷新并编辑输出，再提交给 Agent；另验证中途切换、断开和停止。停止 Agent 不会中断已发出的 Shell 命令，必要时在终端手动中断。保留 IME、全屏程序、Linux/OpenSSH、安装升级及长期稳定性等原有待验收项。


终端绘制基准（手动运行，不计入日常测试）：`cargo test --locked -p rshell terminal_render_benchmark -- --ignored --nocapture`。输出每帧 CPU 时间和 egui shape 数；使用相同构建模式、窗口尺寸和内容对比，不能替代真实窗口输入延迟与 GPU 帧率测量。快照与固定网格回归由 `scripts/test.ps1` 覆盖。


### PowerShell 启动分段测量

运行 `cargo run --locked -p rshell-core --example local_startup_probe`。该探针独立创建 ConPTY，使用 `-NoLogo -NoProfile`，各运行三次：非交互首条脚本标记、非交互显式导入 PSReadLine、默认交互提示符。只运行固定诊断脚本，不读取用户 Profile 或发送 AI 请求；输出为数值 JSON。探针限制输出 64 KiB、每次等待 15 秒，结束后清理测试进程。

2026-10-30 本机结果：

| 观察项 | 三次范围 |
| --- | --- |
| 创建 ConPTY（交互组） | 2.16–2.51 ms |
| ConPTY + 创建进程（交互组） | 15.11–16.78 ms |
| 收到首批字节（交互组） | 29.38–32.27 ms，包含控制序列，不代表可输入 |
| 非交互引擎/主机初始化至首条脚本标记 | 2070.56–2098.88 ms |
| 显式导入 PSReadLine（模块内计时） | 459.61–464.22 ms |
| 默认交互首个提示符 | 2466.14–2492.65 ms |

不同组是独立进程，不能把数值直接相加或把组间差值全部归因于 PSReadLine。首条脚本之前还包含系统进程、运行时与主机初始化，尚未细分到 .NET、磁盘或安全扫描，不能认定这些因素是根因。当前决定保留完整交互功能，改善后台启动、状态显示及 UI 非阻塞；不以隐藏窗口或提前显示“已连接”宣称启动时间缩短。


### Agent 组件交付验证

常规 `scripts/test.ps1` 包含下载回环、取消/校验失败与组件选择框回归。公开 Release 实测可运行 `cargo run --locked -p rshell-core --example component_download_probe -- --download`；`--download` 是此探针明确的下载授权，正式界面必须先选择「下载并使用」。可将当前进程 `LOCALAPPDATA` 指向 `target` 下的隔离目录，避免影响已有用户缓存。探针验证固定文件校验、缓存复用和 SDK `--self-test`，不调用真实模型或执行终端命令。


## GitHub 软件更新验证

- 入口：帮助 → 检查更新。只在用户主动点击时访问 `gooddayday009/rshell` 的 Releases；安装包下载后可点击「安装并重启…」，确认关闭所有会话后，由复制到下载目录的独立 Rust 助手等待原进程退出、重新校验 SHA-256、运行 NSIS 静默安装并重启；也保留打开目录入口。
- 单元及回环验证：`cargo test --locked -p rshell-core update::tests`，覆盖稳定版本数值比较、限定资产地址、校验文件包名、损坏/长度不符的包和取消清理。
- GUI 控件验证：`cargo test --locked -p rshell update_dialog_reports_results_blocks_terminal_and_cancels_work`，不访问公网，验证任务结果、错误提示、取消以及更新弹窗期间终端不接收输入。
- 可选真实下载验证：`cargo run --locked -p rshell-core --example update_probe -- --download`，下载到 `target/update-probe` 并校验 SHA-256，不运行任何安装包；省略 `--download` 只检查版本及资产列表。
- 2026-11-01 已验证：真实 GitHub 的 2.3.3 安装包与便携 ZIP 下载和 SHA-256 校验通过，记录 `target/update-live-probe.log`；`scripts/test.ps1` 通过，记录 `target/update-checks.log`。按用户要求未运行冒烟测试，未启动安装或验证真实窗口/升级流程；现有安装包不包含本次新增入口。
- 资源边界：版本元数据最多 1 MiB，校验文本最多 4 KiB，单包最多 256 MiB；连接超时 15 秒，元数据/校验请求 20 秒，下载 300 秒。SHA-256 用于文件完整性验证，不等于代码签名。下载缓存不会自动清理已验证包，可在退出下载任务后按需删除。

关于窗口：帮助菜单只保留「关于 RShell…」与「检查更新…」。软件名称、编译版本和地址栏说明显示在独立关于弹窗，关闭后回到终端；该弹窗期间不向终端发送输入。

## JumpServer 保存密码与自动认证验证

运行 `scripts/test.ps1 -WithCredentials`：除原凭据测试外，还会运行 `saved_password_auto_auth_mfa_and_failure_stop`，使用随机凭据 ID、合成密码和回环 HTTP 服务，退出时删除测试密码。覆盖保存密码自动认证、MFA 提示、错误验证码后保留重连目标及取消后不继续认证；不连接实际 JumpServer。配置兼容和原账号不可用时停止重连由普通工作区测试覆盖。

2026-11-01：上述检查通过，日志 `target/jumpserver-validation.log`；调试构建以及 JumpServer 配置/MFA 两个单场景窗口验证通过。真实 JumpServer 的令牌到期、MFA 成功后 SSH 重连及网络断线审计仍待现场验收，未打包发布。

自动安装补充：准备更新和校验在后台执行；等待原进程退出最多 120 秒，超时不安装。安装目标是当前程序所在目录（含便携/开发运行目录），NSIS `/S` 静默安装，`/D` 使用当前绝对目录；已下载 ZIP 不作为可执行安装包。失败由更新助手显示错误；真实机器的覆盖安装与会话恢复仍需验收，程序不会自动恢复远端进程或未保存终端状态。

2026-11-01 自动安装补充验证：`scripts/test.ps1` 通过（`target/auto-update-tests.log`），已构建 `target/debug/rshell.exe`。以隔离目录的模拟安装器验证更新助手退出等待、包含空格的安装路径、安装成功后启动新程序，安装/重启标记均生成，目录记录于 `target/auto-update-fixture-result.txt`。该测试不覆盖真实 NSIS 文件替换、注册表与真实会话退出；未运行窗口冒烟或实际安装到用户程序目录。

2.3.4 打包验证（2026-11-01）：`scripts/test.ps1` 通过格式、严格 Clippy 和 217 项测试（4 项原有忽略），Release 构建成功；日志 `target/release-234-checks.log`、`target/release-234-build.log`。按用户要求未运行冒烟或真实安装。产物为 `dist/RShell-2.3.4-windows-x64-setup.exe` 与 `dist/RShell-2.3.4-windows-x64-portable.zip`，各附 SHA-256。

2.3.5 打包验证（2026-11-02）：包含直接导出全部已保存会话的改动。`scripts/test.ps1 -Smoke` 通过格式检查、严格 Clippy、219 项测试（4 项原有忽略）及全部 21 个窗口场景；日志 `target/release-235-checks.log`。Release 构建与安装器生成通过，日志 `target/release-235-build.log`、`target/release-235-installer.log`。产物为 `dist/RShell-2.3.5-windows-x64-setup.exe` 和 `dist/RShell-2.3.5-windows-x64-portable.zip`，各附 SHA-256；已核对安装器版本与 ZIP 内程序一致性。真实安装、Linux/OpenSSH 和 Xshell 客户端兼容性仍待验收，未上传发布。

导出回归检查：从 Xshell 导入缺少用户名的 SSH 会话，分别导出全部 `.rsh` 与 `.xts` 并重新导入，确认会话数量不变、空用户名被保留；连接前仍须补齐用户名。此场景已纳入 `missing_user_is_preserved_and_invalid_host_skipped` 测试。

2026-11-02：空用户名会话导出修复后重新打包 2.3.5。`scripts/test.ps1 -Smoke` 通过 219 项测试（4 项原有忽略）和 21 个窗口场景，日志 `target/release-235-export-fix-checks.log`。安装包与便携包已重建并更新 SHA-256，旧包保存在 `dist/previous-2.3.5-*`。未执行真实安装及真实 Xshell 客户端验收。


## 2026-11-05 终端交互修正

已实现：保留“清除滚动缓冲区，当连接关闭”开关及其默认关闭值，不覆盖用户已保存的选择；关闭该选项时断开保留画面与有界滚动历史，开启时仍清空。滚轮/触控板按 egui 平滑位移与行高累计，不再每帧固定滚动三行。禁用窗口后端的全局剪贴板快捷键拦截，终端 Ctrl+V 发送 0x16；Ctrl+Shift+V、Shift+Insert 和菜单粘贴保留，普通文本输入框恢复系统复制/剪切/粘贴快捷键。启用 eframe `links` 特性，使更新窗口发布页链接能够交由系统默认浏览器打开。

针对性回归：`remote_exit_keeps_shell_target_for_toolbar_reconnect` 使用回环 Telnet 验证断开后画面及历史可搜索，深浅主题 800×600 GUI 帧仍可显示断开状态，并验证开启清空开关后手动断开会清空；`ctrl_v_reaches_terminal_and_explicit_paste_stays_available` 覆盖主屏及备用屏幕的 Ctrl+V 和显式粘贴；`text_fields_keep_native_clipboard_shortcuts` 覆盖输入框剪贴板分流；`smooth_wheel_distance_is_independent_of_frame_count` 验证拆成多个动画帧不会放大总滚动距离；更新弹窗 GUI 测试验证 API 失败后点击发布页仍产生正确 OpenUrl 请求。

边界：上述 GUI 检查使用无 GPU 的 egui 控件帧；真实 Vim/OpenSSH 可视块批量编辑、触控板手感及系统默认浏览器实际启动仍待现场验收。没有修改用户配置，没有打包、发布或替换已安装程序；GitHub API 403 限流不由链接修复消除。

已验证：scripts/test.ps1 全部通过（格式、严格 Clippy、223 项通过 / 4 项原有忽略），日志 target/terminal-interaction-checks.log；cargo check 构建通过，日志 target/terminal-links-build.log。

2.3.7 打包验证（2026-11-05）：scripts/test.ps1 -Smoke 通过格式、严格 Clippy、223 项测试（4 项原有忽略）及全部 21 个窗口场景，日志 target/release-237-checks.log；抽查终端和会话设置截图，清空开关保留且默认未选中。Release 与 NSIS 安装器生成成功，日志 target/release-237-build.log、target/release-237-installer.log。安装包和便携 ZIP 位于 dist/RShell-2.3.7-windows-x64-*，附 SHA-256。原方案文档当前缺失，便携包未包含该文件。未执行真实安装、发布及真实 Vim/OpenSSH 验收。


### SFTP 双栏与续传（2026-11-05）

已实现：左远程/右本地目录、文件夹/文件线框图标、路径输入/上级/刷新、跨栏文件及文件夹拖放、资源管理器拖入远程栏、串行任务队列（最多 500 项）、暂停/继续/移除/清理完成项、递归文件夹和空目录传输。目录与单个递归任务最多 10000 项；链接及特殊文件报错停止，保留先前已完成部分。同名目录合并，同名文件不覆盖，未完成数据保存在任务专属 `.rshell-*.part` 文件中。续传重新扫描目录，并逐字节检查已有前缀；已完成文件也核对数据。暂停等待当前写入块确认。关闭面板后台继续，关闭会话或退出程序会释放内存队列，临时文件保留，当前未实现重启后恢复。

验证入口：`cargo test --locked -p rshell-core --test ssh_loopback sftp`（本机回环服务，双向续传/源变化/目录递归/空目录/不覆盖）；`cargo test --locked -p rshell transfer_view`（目录目标捕获、危险文件名、深浅主题和窄窗口布局）。单场景窗口使用 `--smoke-test --demo --smoke-sftp`，展示明确标记的演示文件和暂停任务，不连接服务器。

已验证：`scripts/test.ps1` 全部通过（格式、Clippy、工作区测试）；双向文件/文件夹跨栏拖放模拟事件、800/1180 像素宽度深浅主题主要操作未裁切；SFTP 单场景原生窗口截图通过。双向暂停续传、变化前缀拦截、已完成文件复核、嵌套及空目录回环测试通过。

待人工验收：真实 Windows 资源管理器拖入、真实 Linux/OpenSSH 大文件/跨网络中断/权限错误、真实目录符号链接、深浅主题实际缩放。回环验证不代表 Linux/OpenSSH 兼容性通过。批量镜像同步、删除目标多余文件与程序重启后恢复队列未实现。


### SFTP 界面反馈修正（2026-11-05）

本地初始位置由后台读取系统桌面目录（UserDirs / Windows Known Folder，兼容重定向）；提供“桌面”快捷入口，显示路径隐藏 Windows 扩展路径前缀，内部路径保留。目录行使用随主题变化的金色线框图标、加粗名称与“文件夹”标记；普通文件显示扩展名和大小。中间竖分隔条调整左右宽度，下方横分隔条调整目录/队列高度，比例在本面板生命周期内保留。队列采用蓝色运行、绿色完成、红色失败、金色暂停、灰色等待标签，附文字，并提供状态计数和筛选。

针对性验证：`cargo test --locked -p rshell transfer_view` 覆盖双向拖放、横竖分隔条真实指针拖动、状态筛选、深浅主题与窄窗口布局。原生截图仍使用标记为测试数据的 `--smoke-sftp` 场景，增加运行、完成、失败和暂停示例；不代表真实任务运行状态。


### SFTP 独立窗口与底边缩放（2026-11-05）

SFTP 从内嵌 egui Window 改为 immediate 原生 viewport，独立标题栏、移动、缩放和关闭。系统窗口外框不依赖内容自动适配高度；队列禁用自动收缩，避免空队列/筛选后底边随内容变化。关闭 SFTP 窗口保留会话内任务，重新打开可查看；退出主程序仍结束任务。打开 SFTP 不再阻塞主窗口终端输入。后端不支持多窗口时才使用 egui 嵌入回退。

本机原生窗口验证：找到独立顶层 HWND（非 WS_CHILD），移动到指定桌面坐标，调整到 900×650，跨帧检查坐标尺寸稳定，并确认主窗口没有跟随移动。证据：`target/test-artifacts/sftp-native-verification.txt`。此验证使用演示数据，不连接生产。真实多显示器不同 DPI、用户所见鼠标拖动手感仍需实际验收。


### SFTP 点击选中与查找修正（2026-11-06）

文件列表改为单一整行点击/拖动响应区，图标、名称、大小和空白均可选中，整行显示主题选中背景。左右栏提供独立的当前目录文件名筛选框（大小写不敏感、中文子串匹配），显示匹配数和无匹配提示；改动查询清除旧选择，筛选后的行保留原条目索引，上传/下载/双击/拖动均对应正确文件。查找不递归遍历子目录，也不搜索文件内容。

针对性测试：`cargo test --locked -p rshell transfer_view` 验证深浅主题图标/名称/大小位置点击、中文与大小写查找、过滤后选择索引、跨栏拖放及原有布局。
