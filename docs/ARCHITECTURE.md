# 目录与代码导航

## 代码入口

| 文件 | 职责 |
| --- | --- |
| `crates/rshell-app/src/main.rs` | 配置目录、单实例锁、Tokio runtime、原生窗口、字体回退和启动错误 |
| `crates/rshell-app/src/cli/` | 独立控制台入口、参数/认证输入、配置选择、输出与退出码；不创建 GUI |
| `crates/rshell-core/src/headless.rs` | 复用 SSH 认证与主机信任的非交互 exec/SFTP；不创建 PTY 或隧道 |
| `crates/rshell-core/src/sftp_sync.rs` | 目录内容清单、SHA-256 比较、同步预览、临时文件校验发布和显式镜像删除；路径、层级与条目数边界 |
| `crates/rshell-app/src/app.rs` | 会话编辑、标签、首次主机密钥自动记录、偏好、最近连接、连接事件接收 |
| `crates/rshell-app/src/terminal_view.rs` | 可见网格绘制、选区、光标、IME、键鼠输入、粘贴确认和查找 UI |
| `crates/rshell-core/src/ssh.rs` | SSH 握手、认证、PTY/shell、保活、取消、有界输入队列和协议事件 |
| `crates/rshell-core/src/tunnel.rs` | 本地/远程 TCP 转发、SOCKS5 握手、有界数据流和取消清理 |
| `crates/rshell-app/src/tunnel_view.rs` | SSH 隧道配置、规则状态及仅隧道标签展示 |
| `crates/rshell-core/src/config.rs` | 会话/主机模型、JSON 版本迁移、备份和原子替换 |
| `crates/rshell-core/src/credentials.rs` | RShell 独立加密文件读写删除、用户级 DPAPI 加解密和内存清理 |
| `crates/rshell-terminal/src/lib.rs` | alacritty_terminal 适配、UTF-8/ANSI 解析、网格快照、回看、搜索、键盘编码 |
| `crates/rshell-core/tests/ssh_loopback.rs` | 本机自动化 SSH 协议集成测试 |
| `crates/rshell-core/examples/local_ssh_server.rs` | 便于用户手动测试 GUI 的本机 SSH 回显服务 |

| `crates/rshell-core/src/connection.rs` | 本地终端与 SSH 共用的输入、事件和取消接口 |
| `crates/rshell-core/src/local.rs` | Windows ConPTY、PowerShell、有界输出、后台读写和退出清理 |
| `crates/rshell-core/src/store.rs` | 串行配置与凭据事务、失败补偿和异步结果 |

## 数据流

```text
原生 GUI → 类型化输入命令 → 有界通道 → SSH 写任务 → 服务器
服务器 → SSH 读任务 → 分块解析 → Terminal 模型 → 可见区域快照 → GUI 绘制
主机密钥 → 可信记录比较 → 首次自动提交后台保存 → 保存成功且连接仍有效 → 恢复握手
```

连接实例与保存的会话配置分离，一个配置可以打开多个标签。每个连接有独立终端和命令通道。连接取消使用独立的 watch 通道，不依赖终端绘制。

SSH 隧道在认证后建立，归属于原始连接工作任务；终端渠道复制不复制其监听。`Tunnels` 持有监听/数据流 JoinSet，取消时回收任务并请求取消远程监听；只接受与已配置远程规则精确匹配的 forwarded-tcpip 渠道。仅隧道模式不请求 Shell，复用原有 SSH 保活。界面接收有界事件队列中的隧道状态，不承担网络 I/O。

终端模块不依赖 egui 或 SSH；核心模块不依赖 GUI。应用层通过 `connection::Terminal` 适配接口绑定两者。协议回复（例如光标位置查询）从终端模型回写所属 SSH channel。

## 本地目录

- `target/local-config/`：`dev.ps1` 默认配置，方便手工测试。
- `target/local-ssh/`：本机测试服务的测试主机密钥。
- `target/test-artifacts/`：窗口冒烟测试截图和测试专用配置。
- `target/debug/`、`target/release/`：Cargo 产物。
- `dist/`：可选便携测试包，不是已发布版本。

不要把测试主机私钥或用户配置复制到 `docs/` 或提交到代码仓库。

## 尚未满足的架构要求

运行中的配置保存和凭据读写已转移到单独的串行后台服务；启动前的配置读取和字体加载仍在窗口创建前进行。分组当前是平面字符串；结构化错误、诊断日志、层级会话树与可配置快捷键也尚待补齐。细项以 [开发状态](STATUS.md) 和方案第 11 节为准。

## 主机记录并发保护

主机记录包含公钥、算法、SHA-256 指纹和 Unix UTC 记录时间。独立更新由后台重新解析公钥并验证指纹，按原记录进行比较后替换，避免较早打开的编辑窗口覆盖已经更新的信任。首次连接由应用自动提交保存；oneshot 发送端交给保存服务，取消/超时会关闭接收端；保存前拒绝失效请求，保存后无法通知原连接时撤销本次信任并报告失败。已记录密钥变化仍阻断。

## 会话文件夹（schema 4）

`rshell-core::config::Folder` 使用 UUID、名称、父文件夹 UUID；`Profile.folder_id` 为可空引用，空值代表根目录。旧 `group` 仅用于读取 schema 1–3 的迁移，不再写出。加载/保存校验父节点、循环、32 层深度、同级重名和悬空会话引用。文件夹 CRUD 与会话移动通过原串行 Store 后台落盘，提交失败保留原内存配置；直接移动会话不调用凭据后端。`rshell-app` 负责递归树、搜索保留祖先、上下文菜单和目录选择。

## 串口数据链路

`rshell-core/src/serial.rs` 使用 serialport 在后台阻塞工作线程打开、配置和读写串口，复用 `connection::{Command, Event, Terminal}`。输入队列容量为 64，事件队列为 16，输出按 8 KiB 缓冲直接交给终端解析器；读取和写入使用有限超时，取消独立于输入队列。退出释放端口后发送 Finished。串口 Resize 仅调整本地网格；终端查询回复沿同一串口返回，远端输出不触发本机操作。

Profile 增加 protocol 和 serial 配置，旧配置缺少字段时默认 SSH。串口参数随会话保存，不走认证界面，不启用 SSH 保活或 SSH 渠道复制。端口枚举由后台线程完成。

文件传输：`rshell-core::transfer` 管理复用 SSH transport 的独立 SFTP channel、后台流式 I/O、目录条目与请求时间限制及临时文件发布；`rshell-core::zmodem` 在 SSH PTY 字节进入终端解析前识别握手，并在传输期间接管该 channel。`rshell-app::transfer_view` 提供 SFTP 原生窗口和后台文件选择；`rshell-terminal` 继续只负责终端解析与网格，不处理文件系统。ZMODEM 结束后将剩余 shell 数据交还终端；传输阶段暂停终端文本保活，避免将文本注入二进制协议。

## XTS 导入与 Telnet

`rshell-core::xts` 在内存读取 ZIP 中的 Xshell UTF-8/UTF-16 会话，只提取白名单配置字段，不执行脚本、不还原主机信任或凭据。文件、条目、文件夹、路径、层级和解压数据均有上限；不把归档成员解压到文件系统。每次导入在配置副本中新建独立根目录，重复导入保留原会话，新增 UUID。`Store::ImportXts` 在串行后台读取并原子保存，成功后才返回 `Outcome::Imported` 和分类统计；失败不发布新配置，也不调用凭据后端。GUI 仅选择路径、提交请求和显示结果。

`rshell-core::telnet` 复用 `connection` 抽象，使用 Tokio TCP 和有界输入/事件通道（64/16）。连接超时 15 秒，写入超时 10 秒；取消覆盖连接、读写和等待阶段。流式 IAC 状态机支持分片协商、BINARY、服务端 ECHO、SGA、TTYPE、NAWS，子协商限制 1024 字节。NVT 输入回车编码为 CRLF，输出 CR-NUL 去除 NUL，IAC 字节转义；窗口尺寸按 NAWS 协商后发送。远端输出只交给终端解析器，不触发本机文件操作。Telnet 认证由用户在远端提示后输入，不调用 SSH 认证、主机信任或凭据存储；暂不提供本地行编辑/回显模式。
