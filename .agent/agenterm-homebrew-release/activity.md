# Activity

## 2026-09-13

- 用户批准按 ScreenOff 模式推进 Homebrew 分发，正式 tag 前保留 Release Gate。
- 已确认首发域名为 `https://agenterm.nickxu.me`，Bundle ID 为 `me.nickxu.agenterm`，
  首发版本假设为 `v0.1.0`，仅 Apple Silicon，使用 ad-hoc 签名。
- 本地基线 release bundle 成功：`Agenterm.app`、DMG 与 arm64 zip 已生成并通过签名检查。
- 输入框 Cmd+C 回归已通过用户手测；原版 Warp 的动态菜单 action updater 已恢复并提交为
  `44089c2`，尚未推送。
- 使用 task-planning 创建 5 个实现任务，任务 1 开始。
- `./script/build_macos_release 0.1.0` 已生成 arm64 zip，并验证 Bundle ID、版本、图标与 ad-hoc 签名。
- 使用临时本地 tap 完成 cask 安装、caveats、bundle 元数据、签名与卸载演练。
- `./script/format`、目标 nextest、`cargo clippy -p warp --all-targets --tests -- -D warnings`、
  `brew style` 与 Ruby 语法检查通过。
- 发布前审查补齐手工重跑已有 tag 时从 tag 源码构建的约束，并统一隐藏本地 Drive 的 Share 入口。
- 用户批准公开 Agenterm 仓库并发布 `v0.1.0`。
