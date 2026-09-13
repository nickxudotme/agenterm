# Plan：通过 Homebrew Tap 分发 Agenterm

## 目标

复用 ScreenOff 已验证的发布模式，为 Agenterm 建立可重复的 macOS GitHub Release 与
`nickxudotme/homebrew-tap` Cask 分发链路，并在安装完成后显示中英文首次启动提示。

## 当前状态

- 状态：in_progress
- 当前步骤：正式发布与发布后验证
- 上次同步：2026-09-13，用户批准公开仓库并执行发布
- 下一步：提交、配置 tap deploy key、推送 `v0.1.0` 并验证 Homebrew 安装

## Scope 与边界

- In scope：Agenterm 的 tag/manual release workflow、Apple Silicon `.app` zip、SHA-256、GitHub
  Release、tap 中 `agenterm` cask、自动更新 tap、安装后的中英文 caveats、从 release artifact
  进行一次真实 `brew install --cask`/启动文件检查；产品主页使用
  `https://agenterm.nickxu.me`，macOS Bundle ID 使用 `me.nickxu.agenterm`。
- Out of scope：Intel/universal 构建、Windows/Linux 分发、Sparkle 自动更新、Mac App Store、商业
  Developer ID 证书采购。
- 假设：首个正式版本使用 `v0.1.0`；当前无 Developer ID 与 notarization 凭据，因此沿用
  ScreenOff 的 ad-hoc 签名方式，并明确提示首次启动可能需要移除 quarantine。
- 关键约束/不变量：release artifact 必须来自 tag 对应源码；cask 的版本、URL 与 SHA 必须指向同一
  artifact；workflow 缺少 tap deploy key 时 release 仍成功并给出明确提示；不得把私钥写入仓库；
  cask 安装到 `/Applications/Agenterm.app`，卸载不删除用户数据，除非已有明确可验证的偏好路径；
  正式发布前 Agenterm GitHub 仓库必须公开，否则 Homebrew 无法匿名下载 Release artifact。
- 需要重新获批的变化：新增付费签名/公证服务、扩大到 Intel/universal、改变首发版本、发布前发现
  必须引入新的外部制品托管或长期 secret。

## 成功标准

- [ ] 推送 `v0.1.0` 或手工指定 `0.1.0` 可构建并验证 `Agenterm.app`，生成版本化 zip 和 SHA-256。
- [ ] workflow 创建 GitHub Release，并通过仅对 tap 有写权限的 deploy key 自动更新
  `Casks/agenterm.rb`。
- [ ] `brew install --cask nickxudotme/tap/agenterm` 可安装 app；`brew uninstall --cask agenterm`
  可正常卸载。
- [ ] 安装结束显示与 ScreenOff 风格一致的中英文 caveats，包含 Gatekeeper/quarantine 处理命令和
  Agenterm GitHub 地址。
- [ ] cask 通过 Ruby 语法、`brew audit --cask --new-cask`（若自有 tap 规则允许）及本地 artifact
  安装验证；app 内可执行文件、bundle id `me.nickxu.agenterm`、版本字段与图标存在，cask
  homepage 指向 `https://agenterm.nickxu.me`。
- [ ] 发布配置通过格式、shell/Ruby 静态检查；Agenterm 相关构建检查不因发布改动回归。
- [ ] 最终交付包含 release URL、tap commit、实际安装输出摘要、安装命令和未签名/未公证风险。
- [ ] 最终 artifacts 反映真实状态：完成、跳过、阻塞和验证结果。

## Artifact 决策

- `spec.md`: 不需要 — 不改变产品运行时行为，发布契约完整记录在 plan、workflow 和 cask 中。
- `tasks.md`: required — 实现跨 Agenterm 与 homebrew-tap 两个仓库，并包含构建、发布、secret 与
  真实安装验证等有依赖关系的步骤。

## 质量门禁

- [x] Gate: Plan Approval — 2026-09-13 用户确认“我们推进 brew 分发”。
- [x] Gate: Code Review — 2026-09-13 完成 diff、构建脚本、workflow、cask 与本地行为审查。
- [x] Gate: Release — 2026-09-13 用户确认仓库可公开并批准“走发布”。
- [ ] Gate: Final Review — 审核 release、tap 更新和实际 brew 安装证据。

## 步骤

1. [x] 调研 ScreenOff release workflow、tap cask caveats、Agenterm macOS bundle 与当前凭据状态。
2. [x] 调用 `task-planning` 创建 `tasks.md`，拆分两个仓库的实现与验证依赖。
3. [x] 完成 `tasks.md` 中的实现任务。
4. [x] 本地构建并验证版本化 Agenterm artifact；对 cask 做本地 URL/SHA 安装演练。
5. [x] 代码评审 → Gate: Code Review。
6. [~] 准备两个仓库的提交、tap deploy key 配置步骤和 `v0.1.0` tag → Gate: Release。
7. [ ] 发布后回读 GitHub Release 与 tap，执行干净的 brew 安装/卸载 smoke test。
8. [ ] 汇总证据并更新 artifacts → Gate: Final Review。
