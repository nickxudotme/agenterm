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
- 首次 release run `34744450173` 在旧 Xcode 不支持 `-downloadComponent` 时失败；workflow 改为
  按能力下载 Metal toolchain，旧 Xcode runner 则验证预装的 `metal` 与 `metallib`。
- GitHub ARM runner 的第二次冷构建耗时过长，用户批准切换为本地制品；该 run 已取消。
- 从独立 `v0.1.0` worktree（`c548129d6a2a102301f3b4a25775af5c75845b7b`）复用 Cargo 缓存完成
  正式构建，总耗时约 10 分钟，ZIP SHA-256 为
  `2829594f2660c9820f2b3551f655ca4a11a18ef43bed7d401bf30b0ac973fdca`。
- GitHub Release `v0.1.0` 已公开，远端 asset digest 与本地 SHA 一致；tap 更新到 commit
  `f3e8da8`，并跳过会被旧测试 tag 干扰的 livecheck。
- 从公开 Release URL 执行 `brew install --cask nickxudotme/tap/agenterm` 成功，中英文 caveats
  正常显示；安装后的 Bundle ID、版本、arm64 架构及 ad-hoc 签名通过验证。
- `/Applications/Agenterm.app` 已成功启动，Agenterm 主进程与 terminal server 均正常运行。
- `brew audit --new --cask` 的下载和校验通过，剩余失败仅为新仓库关注度门槛及缺少 Apple
  Developer ID 签名，符合首发已知边界。
