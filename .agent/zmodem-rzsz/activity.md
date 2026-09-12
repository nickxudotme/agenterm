# Activity

## 2026-09-12：sz / rz（ZMODEM）支持

### 环境与分支

- 新建 worktree `/Users/nickhaoxu/my-stdio/agenterm-zmodem`，分支 `feat/zmodem-rzsz`，基线 `28cc180`（干净）。主 worktree 有他人未提交改动，未触碰。
- 远端 mini（Apple M4，10 核）用于编译：目录 `/Users/nx/agenterm-zmodem`，与另一 agent 在用的 `/Users/nx/agenterm` 隔离。warp_terminal 增量 check 约 2-15s。
- mini 缺 macOS SDK bindgen 头文件，`-p warp`（GUI）只能本机编译；warp_terminal 层可在 mini 上验证。

### 实现

- `crates/warp_terminal/src/zmodem.rs`：wire 原语（CRC-16/XMODEM、CRC-32/ISO-HDLC、ZDLE 反转义、ZHEX 头编解码）刻意与 `zmodem2` 一致，探测器不会声称状态机会拒绝的传输。`ZmodemDetector` 要求完整合法头（含 CRC）才接管；`DetectorOutcome::Render` 保证未接管字节原样渲染，跨读边界的半个头会被暂存而不是当乱码喷出。
- `ZmodemSession` 封装下载（`Receiver`）与上传（`Sender`），poll/submit 驱动，不做任何阻塞 I/O。
- 事件循环 `route_zmodem` 以自由函数实现，放在取 `TerminalModel` 锁之前，避免跨锁持有借用；失败一律回退渲染，不吞输出。
- 下载落盘走 `ZmodemTransfer` + 原生保存对话框；上传走 Command Palette `terminal:send_files_with_zmodem`（`rz` 需对端先起，无法自动探测）。
- `remote_tty` 的网络 PTY 明确拒绝上传而不是假装开始。

### 验证证据

- `warp_terminal` 单测 20/20；`warp` 侧 `zmodem_transfer` 7/7。
- **真实 lrzsz 端到端**：`crates/warp_terminal/tests/zmodem_lrzsz.rs` 用 PTY 驱动真实 `sz`，经 `ZmodemSession` 接收，2/2 通过：文本文件字节一致；8 KiB 二进制（含全部 256 种字节值、跨多个 subpacket、走 ZDLE 转义路径）字节一致。耗时从 60s 优化到 ~4s（非阻塞读 + 主动结束 `sz` 的重试等待）。
- 早期用 `/tmp/zm/harness` 独立 harness 时出现 19456/20000 字节差，根因是 harness 在 `SessionCompleted` 时提前 break 丢掉尾部 subpacket；生产代码的 `submit_wire` 不提前 break，因此不受影响，仓库内集成测试证实完整。
- `./script/format`、`git diff --check`、`check_no_inline_test_modules` 通过；`cargo clippy -p warp_terminal --lib --all-features --tests -- -D warnings` 与 `-p warp --lib --tests` 均 0 error。
- GUI `./script/run --dont-open` 构建、打包、签名成功，`codesign --verify --deep --strict` 通过，已启动运行。

### 预先存在的失败（非本次引入，已用干净基线 worktree 对照确认）

- `cargo test -p warp --lib terminal::` 并行下 6 个失败（test_insert、test_insert_into_input、test_reinput_blocks、test_scroll_position_doesnt_change_when_block_finished、test_viewport_iter_most_recent_at_bottom、open_in_warp::test_single_code_file）。干净基线 `28cc180` 出现完全相同的 6 个；单独串行跑全部通过。本分支 1284 passed vs 基线 1277（多出 7 个 zmodem 测试）。
- `cargo clippy --workspace --all-targets` 在 `app/src/ai/execution_profiles/profiles_tests.rs` 报 unresolved import / missing method；干净基线同样报错。

### 未完成 / 待确认

- 未做 GUI 内真实 `sz` 的人工可视验收（CUA attach 超时）；协议与落盘链路已有仓库内真实 lrzsz 证据，但"用户在 Agenterm 窗口里跑 sz 看到保存对话框"这一步仍待用户确认。
- 进度百分比/取消按钮的 UI 面板未实现，当前以 toast 报告开始、完成与失败。
- 未提交推送到远端，未合并。
