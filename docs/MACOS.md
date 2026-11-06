# macOS 支持

已实现源码适配：原生 Rust/egui 窗口、系统 zsh 登录交互终端、SSH/SFTP 共用连接层、中文字体回退、Command+C/V 复制粘贴、macOS 钥匙串保护、按 CPU 架构匹配更新 ZIP。目标为 macOS 12+，Apple Silicon 与 Intel 分别构建。

Windows 专用的 Agent SDK“会执行”选项及组件下载入口在 macOS 不显示；通用 AI 问答、命令建议及经确认执行单行草稿仍可使用。macOS 不调用 Windows 安装器，更新 ZIP 需退出应用后手动替换。

## 构建与安装

在 Mac 上安装 Xcode Command Line Tools 与 Rust stable，然后在项目根目录执行：

```sh
bash scripts/test-macos.sh --credentials --smoke
bash scripts/build-macos.sh arm64  # Apple Silicon
# Intel Mac 使用 bash scripts/build-macos.sh x64
```

产物为 `dist/RShell-<版本>-macos-arm64.zip` 或 `dist/RShell-<版本>-macos-x64.zip`，附 SHA-256 文件。将 ZIP 中的 `RShell.app` 拖入“应用程序”。CLI 位于 `RShell.app/Contents/MacOS/rshell-cli`。

打包脚本使用 ad-hoc 签名，尚未接入 Developer ID 签名、公证或正式发布。下载包的 Gatekeeper 首次启动流程仍需实机验收，不应据此宣称安装验收通过。

## 本地终端与凭据

本地终端固定启动 `/bin/zsh -l -i`，设置 `TERM=xterm-256color`，会加载用户 zsh 初始化文件；连接状态代表进程已启动，不保证用户初始化脚本已完成。远端 SSH Shell 不由本地系统推断。Ctrl+C 保留终端中断，Command+C 复制选区，Command+V 粘贴，多行粘贴仍需确认。

默认配置目录由 `directories::ProjectDirs` 按 macOS 规则定位，也可使用 `RSHELL_CONFIG_DIR` 指定隔离目录。会话密码、模型 API Key、托管私钥和聊天历史文件使用 AES-256-GCM；随机主密钥保存在当前用户钥匙串，UUID 绑定作为认证数据。JumpServer 密码和令牌直接存入钥匙串。访问拒绝或解锁失败会返回错误，不回退到明文。钥匙串与加密文件需同时保留；Windows DPAPI 文件不能直接复制到 Mac 解密，需重新录入密码并通过带口令的私钥导出/导入迁移。

在可交互且已解锁钥匙串的 Mac 上，执行 `bash scripts/test-macos.sh --credentials` 补做凭据专项测试；常规回归中的聊天持久化测试也会访问钥匙串，可能提示授权。测试会创建应用主密钥和临时测试凭据。CI 使用独立临时钥匙串运行专项测试，并执行 `--smoke` 全套窗口冒烟；任务结束删除临时钥匙串。

## 验证边界

- 已实现：源码平台分支、测试与 `.app`/ZIP 打包脚本，双架构 GitHub Actions 工作流。
- 已验证：Windows `scripts/test.ps1 -WithCredentials` 全部通过；本地终端和深/浅主题 AI 侧栏三个窗口冒烟通过；Bash 脚本语法检查通过。测试日志：`target/macos-support-windows-tests.log`。
- 本次本地验证环境是 Windows；Mac 的编译、钥匙串授权、GUI、实际 SSH/SFTP、zsh 子进程退出、中文输入法、拖放、深浅主题与窄侧栏均须在 Mac 单独验证。
- CI 文件存在不表示 CI 已通过；未提供已验证的 Mac 安装包。性能、全屏应用和原方案中未完成的验收项继续开放。

CI 架构标签依据 [GitHub Runner Images](https://github.com/actions/runner-images)；钥匙串调用使用 [security-framework](https://docs.rs/security-framework/3.7.0/security_framework/os/macos/keychain/struct.SecKeychain.html)。
