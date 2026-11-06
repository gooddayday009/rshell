# 无界面 SSH / SFTP CLI

`rshell-cli.exe` 是独立 Rust 控制台入口；不会创建 GUI、PTY 或交互 Shell。GUI 仍由 `rshell.exe` 启动。网络、磁盘和凭据工作使用 Tokio / 后台线程，stdout/stderr 流式输出，不累计整份命令输出。当前只支持 SSH 服务，不支持 Telnet、串口或 JumpServer 资产选择。

## 构建与帮助

```powershell
cargo build --locked -p rshell --bin rshell-cli
.\target\debug\rshell-cli.exe --help
.\target\debug\rshell-cli.exe --version
```

`scripts/build.ps1` 同时构建 GUI 和 CLI；2.3.10 起的 ZIP 和 NSIS 安装包包含 CLI 及本说明，不修改 PATH。真实安装、升级验收仍待完成，既有 2.3.9 产物不包含本次 CLI 改动。

## 使用已保存的会话

**先在 RShell GUI 的“设置 → 会话”勾选“启用命令行（CLI）”。**新配置及缺少该字段的旧配置均默认关闭。开关针对所选配置目录保存；下面的命令示例只有在对应配置已显式启用时才执行。

以下示例的“测试服务器”应替换为自己的 SSH 会话完整名称或 UUID；名称重复时必须使用 UUID，可从 `config.json` 的 `profiles[].id` 查看。未提供标准输入密码时，读取该会话已有的加密凭据；无凭据的密码会话会立即报错，不弹出输入窗口，也不把密码写入配置或日志。

```powershell
# 完整远端命令作为 -- 后的一个参数，原样发送到远端 SSH exec
.\target\debug\rshell-cli.exe exec --profile '测试服务器' -- 'uname -a'
$LASTEXITCODE

.\target\debug\rshell-cli.exe sftp ls --profile '测试服务器' --json -- '/tmp'
.\target\debug\rshell-cli.exe sftp upload --profile '测试服务器' -- 'D:\data\report.txt' '/tmp/report.txt'
.\target\debug\rshell-cli.exe sftp download --profile '测试服务器' -- '/tmp/report.txt' 'D:\data\received.txt'
```

PowerShell 单引号包住命令可避免本机变量和表达式提前展开。CLI 不拼接多个命令参数，也不推断远端 Shell、权限或工具。远端输出原样写入 stdout/stderr；CLI 不执行输出中的本机操作。命令只通过 SSH `exec` 请求发送；不附加回车、不使用终端解析器、不触发 ZMODEM。远端 stdin 关闭，因此需要交互输入或 TTY 的程序不适用。

## 直接连接与认证

直接连接要求同时指定 `--host` 与 `--user`，端口默认为 22；可加 `--identity` 使用私钥。`--profile` 不能与 `--host/--user/--port/--identity` 混用，防止把一个会话的已保存凭据发送到另一目标。

```powershell
.\target\debug\rshell-cli.exe exec --host 192.0.2.10 --user operator --port 22 --identity 'D:\keys\test_ed25519' -- 'id'
```

192.0.2.10 为文档示例地址，请替换为有权限访问的目标。无口令私钥不需要额外输入。密码或加密私钥口令使用 `--password-stdin`，只读取标准输入的第一行，要求 UTF-8，最多 16384 字节，去除末尾 LF/CRLF、保留其他空格。不支持命令行明文密码参数，也不接受在会回显的控制台中直接输入。可由自己的凭据工具通过管道提供该行；不要将真实密码写进命令历史或脚本。

PowerShell 7 的交互示例（既有凭据工具可替代此输入过程）：

```powershell
$OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$secret = Read-Host 'SSH 密码或私钥口令' -MaskInput
try {
    $secret | .\target\debug\rshell-cli.exe exec --host 192.0.2.10 --user operator --password-stdin -- 'id'
} finally {
    Remove-Variable secret
}
```

上述 PowerShell 示例在短暂的进程内存中使用明文变量；CLI 不保存新密码。当前仅复用 GUI 已有的 Windows DPAPI 加密凭据读取实现，不改变凭据存储方式。SSH Agent、键盘交互/MFA 和密码交互提示尚未实现。

## 配置与主机密钥

配置目录优先级为 `--config-dir` > `RSHELL_CONFIG_DIR` > GUI 的默认用户配置目录。CLI 使用其中的 `config.json` 和现有凭据。首次遇到主机密钥时，验证公钥与指纹、取得 `instance.lock`、重新读取配置并保存信任，保存成功才继续认证；写锁不可用或落盘失败即停止。GUI 正在使用同一目录时，未知主机需先关闭 GUI 后重试；已记录且匹配的主机可以与 GUI 并行连接。密钥变化始终阻断，没有跳过校验参数。

CLI 不创建/修改保存的会话，不更新时间或偏好；仅首次主机信任需要写配置。已保存的隧道与空闲发送字符串不在 CLI 中启用。

## 默认关闭与安全边界

- CLI 执行 SSH 命令或任何 SFTP 操作前读取开关；关闭时返回 255，在读取已保存凭据、密码标准输入以及建立 SSH 连接之前拒绝。`--help`、`--version` 仍可用；没有 `--enable` 或跳过检查的命令行参数。
- 开关是 `config.json` 中默认值为 false 的 `cli_enabled`，由设置界面后台保存。关闭阻止新调用，不会中止已经启动的任务。`--config-dir` 或 `RSHELL_CONFIG_DIR` 选中的配置同样必须显式启用；新目录不会自动开启。
- CLI 是主动连接服务器的客户端，不创建 HTTP 服务或网络监听端口；即使保存的会话配置包含 SSH 隧道，CLI 也不会启动这些监听。
- 首次主机密钥按项目规则自动记录，后续变化会阻断；首次记录本身不能证明服务器身份。首次连接应使用可信网络，并通过可信渠道核验服务器指纹。
- 这不是 Windows 用户权限之外的安全隔离。已经取得同一 Windows 用户权限的恶意程序可能修改配置、调用已启用的 CLI，或使用该用户可解密的已保存凭据；此开关不能保证系统不会被入侵。启用即允许当前用户通过脚本使用相应 SSH 账号的权限，远端命令不会再弹 GUI 确认。
- 默认关闭、主机密钥变化阻断、密码不进入命令参数、明确的同步方向及显式 `--delete` 是本次实现的限制；没有完成独立安全审计或渗透测试。真实凭据与敏感目录仍需按操作系统账户和远端账号权限管理。

## 输出、退出码和传输边界

| 操作 | 输出 / 退出码 |
| --- | --- |
| `exec` | stdout 与 stderr 分开，成功收到退出状态时返回远端原码（Windows 32 位）；非零即远端失败 |
| `sftp ls` | 默认 TYPE/SIZE/NAME 列表；控制字符转义；`--json` 返回 path、entries 数组 |
| `sftp upload/download` | 成功无标准输出，退出码 0；错误在 stderr |
| `sftp sync upload/download` | 输出操作列表；`--json` 返回方向、根目录、dry_run/delete 标志及 operations；成功 0 |
| 用法错误 | 2 |
| 超时 | 124 |
| Ctrl+C | 130 |
| 连接、认证、传输、输出写入失败或无远端退出码 | 255 |

远端也可能返回 2/124/130/255；诊断时结合 stderr 判断。全操作默认 300 秒，可用 `--timeout 1..86400` 调整；内部连接和 I/O 还有更短的分阶段超时。取消或超时关闭 SSH 连接，但不保证远端进程已经终止；命令不会自动重试。Ctrl+C 监听已实现，真实控制台按键验收仍待完成。

`sftp upload/download` 只支持单个普通文件，源不能为目录、符号链接或特殊文件。源、目标都是完整路径，目标父目录必须存在。传输沿用 64 KiB 缓冲与临时 `.rshell-*.part` 文件；完成后发布，同名目标不覆盖。失败、取消或超时可能留下临时文件，须核对后自行清理；没有跨进程续传。目录列举最多 10000 项。递归及更新同名文件使用下面的 `sync`，文件权限/时间戳保留和 stdin 命令输入尚未实现。

## 目录同步

目录同步是一次性的单向操作，将**源目录中的内容**同步到目标根目录，不额外套一层源目录名。支持嵌套目录、空目录、中文及空格文件名。按两端文件的长度及完整 SHA-256 内容比较，跳过内容相同的文件，新增缺失文件，更新内容不同的同名文件；不是只按修改时间判断，也不是持续监控或双向冲突合并。

```powershell
$cli = '.\target\debug\rshell-cli.exe'

# 先预览上传差异，不写入同步目录
& $cli sftp sync upload --profile '测试服务器' --dry-run --json -- 'D:\data\source' '/srv/backup'

# 上传同步，默认保留目标端多余文件和目录
& $cli sftp sync upload --profile '测试服务器' --timeout 1800 -- 'D:\data\source' '/srv/backup'

# 下载同步，同样默认保留本地目标多余内容
& $cli sftp sync download --profile '测试服务器' -- '/srv/backup' 'D:\data\received'

# 可选镜像删除：建议先查看待删除的具体相对路径
& $cli sftp sync upload --profile '测试服务器' --dry-run --delete -- 'D:\data\source' '/srv/backup'
& $cli sftp sync upload --profile '测试服务器' --delete -- 'D:\data\source' '/srv/backup'
```

`--delete` 仅用于 `sync`，删除方向始终指向目标端：upload 删除远端目标的多余条目，download 删除本地目标的多余条目。没有该参数就不删除。空源目录加 `--delete` 会清空目标目录内的普通文件和子目录，但保留目标根目录。文件与目录同名冲突不会通过删除来解决，而是在执行前报错。

普通输出包括 PLAN/DONE 标志和操作表；JSON 的 `operations` 包含 `kind`、`path`、`bytes`。`kind` 为 `mkdir/create/update/skip/delete_file/delete_dir`；`path` 相对于所选根目录，空字符串表示根目录本身。`--dry-run` 会连接服务器、读取两端完整文件内容计算差异，但不创建或更新同步目录、不删除文件；首次连接的主机信任配置仍按前述规则保存。大目录可能需要增加 `--timeout`。

同步前完整扫描两端目录。单棵目录树最多 10000 项（含根目录）、32 层子路径，相对路径最多 4096 UTF-8 字节；传输、哈希使用 32 KiB 数据块。目标根可以不存在，但其父目录必须存在；远端根必须为绝对路径。根路径不接受 `..`；不跟随符号链接、Windows 重解析点或特殊文件，拒绝 Windows 不安全文件名及大小写冲突。镜像删除不允许使用本地盘符/共享根或远端 `/` 作为目标。

每个新增/更新文件先写入目标同目录的 `.rshell-sync-*.part`，核对内容、源文件及原目标后发布。上传替换已有文件要求服务端声明 `posix-rename@openssh.com` v1；不支持时正式同步在修改文件前报错，预览仍可使用。扩展采用 [OpenSSH 官方协议定义](https://github.com/openssh/openssh-portable/blob/master/PROTOCOL)；客户端不会先删除旧文件再上传。新文件沿用不覆盖发布，下载替换使用本地同目录重命名。当前不保留原权限或时间戳，更新可执行文件等场景需自行核对权限。

只有新增、更新全部完成且重新扫描确认两端未变化后，才执行镜像删除；逐项复核后先删文件、再删空目录，不调用递归删除。扫描错误、传输失败、内容变化或取消会终止操作。同步不是整棵目录的事务或文件系统快照：中途失败时先前完成的条目仍保留，临时文件可能残留，已完成的删除不能回滚；操作期间应停止其他进程修改源/目标目录。进程取消、连接断开或异常退出后不自动重放、不跨进程续传。

## 验证

```powershell
cargo test --locked -p rshell-core --test headless
cargo test --locked -p rshell-core --test sftp_sync
cargo test --locked -p rshell --bin rshell-cli --test cli
.\scripts\test.ps1
```

测试使用回环地址、随机端口、虚构认证和 `target/test-artifacts` 下的临时配置。覆盖跨进程命令/输出/退出码、SFTP 二进制往返、拒绝覆盖、主机密钥信任与变更、认证失败、超时及连接释放；本机 russh 服务不代表真实 Linux/OpenSSH 兼容性验收通过。当前验证结果与待完成项见 [开发状态](STATUS.md)。
