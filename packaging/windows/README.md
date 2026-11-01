# Windows 构建与打包

本目录保存 Windows 交付说明。`scripts/build.ps1` 构建 release exe 与便携 ZIP；`scripts/build-installer.ps1` 使用按 zlib/libpng 许可开放商业使用的 NSIS 生成 2.3.4 Windows x64 安装包。

```powershell
.\scripts\test.ps1 -Smoke
.\scripts\build-installer.ps1 -MakensisPath 'C:\Program Files (x86)\NSIS\Bin\makensis.exe'
```

安装包在 `dist/RShell-2.3.4-windows-x64-setup.exe`，同目录有 SHA-256 文件。按当前用户安装到 `%LOCALAPPDATA%\Programs\RShell`；通过 Windows“已安装的应用”卸载。安装、升级和卸载不删除用户配置或 DPAPI 加密凭据。安装器版本来自 workspace `Cargo.toml`，安装脚本为 `packaging/windows/rshell.nsi`。如 NSIS 不在默认路径，可用 `-MakensisPath` 指定。安装器会先卸载本机已安装的 1.0.0 Inno 版本。

如果已有窗口占用 `target/release/rshell.exe`，可加 `-TargetDirectory target/windows-next` 在独立目录构建。1.0.0 Inno 升级路径尚待实机验证，升级前请备份配置。

安装包未签名；完整第三方依赖许可证清单、安全审查、干净 Windows 环境及目标 SSH/终端兼容性验收尚未完成。

```powershell
.\scripts\test.ps1 -Smoke
.\scripts\build.ps1 -Package
```

产物：

- `target/release/rshell.exe`
- `dist/RShell-<版本>-windows-x64-portable/`
- 同名 `.zip` 和 `.zip.sha256`

构建脚本不修改全局 Cargo 配置，不签名、不发布，也不安装到系统目录。产物不包含用户配置、私钥、缓存和测试主机密钥。便携包及安装器均附带项目的 `LICENSE`；Geist 字体嵌入 exe，便携包 `licenses/Geist-OFL.txt` 附带其 SIL Open Font License 1.1。Lucide SVG 嵌入原生界面，便携包与安装器均附带 `licenses/Lucide.txt`，保留其 ISC 及来源许可声明。

当前以 `stable-x86_64-pc-windows-msvc` 为已验证开发工具链。NSIS 安装器提供安装快捷方式和卸载入口；MSI、代码签名、自动升级和干净 Windows 环境验收尚未完成，不能把 ZIP 当作安装验收完成的证据。

第三方许可证清单与安全审查仍待发布前补齐；ZIP 用于项目本地测试。


## 按需 Agent SDK 组件

默认构建只编译 Rust 主程序，不要求 Python。默认便携包与安装器不包含 Agent 目录；`scripts/dev.ps1` 也不再预先构建组件。执行型 Agent 缺少组件时先弹框，选择下载后从 GitHub Release 获取固定组件，下载校验和用户缓存流程见 [AGENT_COMPONENT.md](../../docs/AGENT_COMPONENT.md)。

离线完整包使用 `scripts/build.ps1 -Package -WithAgent` 或 `scripts/build-installer.ps1 -WithAgent`；这时构建机才需要 Python 与 SDK 构建依赖。完整包文件名带 `full`，与轻量包分开。

组件单独发布使用 `scripts/package-agent-sdk.ps1`，生成独立 ZIP 和固定版本描述 `agent-sdk/component.json`。必须发布描述中对应的精确 ZIP，验证匿名下载，再编译主程序。不要替换已有版本的 Release 资产；组件更新使用新版本号。SDK、Python 与配套引擎适用各自上游许可，组件保留第三方许可和依赖清单。
