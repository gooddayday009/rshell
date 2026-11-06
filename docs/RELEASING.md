# GitHub 自动发布

`.github/workflows/release.yml` 在推送 `v*` Tag 时启动 Windows x64 发布。只接受与 `Cargo.toml` workspace 版本完全一致的稳定版本 Tag，例如 `v2.3.11`；预发布版本暂不支持。普通分支提交不触发此工作流。

## 发布步骤

1. 更新 `Cargo.toml` workspace 版本、`Cargo.lock` 及版本说明，检查并提交需要发布的代码。Tag 指向的提交必须包含工作流和所有本版改动。
2. 推送分支，再创建并推送版本 Tag（以下版本仅为示例，发布前确认尚未使用）：

```powershell
git push origin HEAD
git tag -a v2.3.11 -m "Release v2.3.11"
git push origin v2.3.11
```

3. 在 GitHub Actions 查看 `Release Windows`。工作流安装 Rust stable 和 NSIS，运行 `scripts/test.ps1`（格式检查、严格 Clippy、常规测试），再复用现有脚本构建便携 ZIP 和安装 EXE。GitHub 托管环境不运行窗口冒烟测试；测试线程数设为 2，减少既有 GUI 单元测试的并发压力。
4. 工作流校验两个包的 SHA256，创建草稿并上传两个包及各自的 `.sha256` 文件，上传成功后自动公开 Release。发布说明由 GitHub 根据提交/PR 自动生成，必要时发布后补充人工验收边界。

仓库必须启用 GitHub Actions，并允许工作流使用 `contents: write`。使用 GitHub 自动提供的 `GITHUB_TOKEN`，无需添加个人访问令牌。组织策略仍可能限制权限。测试或构建失败会阻止发布。需要窗口冒烟验证时，在本地运行 `scripts/test.ps1 -Smoke`。

## 失败与重试

- 测试或构建失败：查看失败步骤日志，修复后使用新版本 Tag；不要移动已经公开发布的 Tag。
- 创建/上传失败可能留下草稿；确认没有公开发布后，可删除该失败草稿，再重新运行同一次 Actions。工作流不覆盖已有 Release 或资产。
- 上传成功但公开失败：检查草稿包含四个附件且校验正确后，可在 GitHub 手动公开。
- 不要将同一版本重新构建的包覆盖到已发布版本；修复应使用新版本号。

## 范围与验收

默认发布轻量安装包和便携包，不包含 Agent SDK。组件独立发布流程见 [Agent 组件说明](AGENT_COMPONENT.md)。离线完整包不在本工作流范围内。

安装包仍未签名；SHA256 仅校验文件一致性，不提供发布者身份认证。自动构建和本机 russh 测试不替代真实 Linux/OpenSSH、干净 Windows 安装升级、真实输入法及全屏应用等人工验收。

工作流配置已实现；首次 GitHub 托管构建、Release 上传和匿名下载校验仍需线上验证。
