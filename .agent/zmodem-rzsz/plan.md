# Plan：Agenterm 支持 sz / rz（ZMODEM 文件传输）

## 目标

在 Agenterm 本地终端里支持 `sz`（从远端下载文件到本地）和 `rz`（从本地上传文件到远端），
参照 WeTERM 的 `zmodem/ssh-zmodem-filter` 做法：在 PTY 字节流上做 ZMODEM 探测与接管，
接管期间把协议字节从终端渲染中剥离，由客户端完成真正的文件 I/O 和本地文件选择。

## 当前状态

- 状态：planning
- 当前步骤：已完成代码调研，产出本 plan
- 上次同步：2026-09-12，创建 plan
- 下一步：Gate: Plan Approval

### 调研结论（事实，供后续步骤直接引用）

**WeTERM 参考做法**（`/Users/nickhaoxu/my-stdio/WeTERM`）：

- 后端是独立进程，`ssh2` 输出**先经过 `zmodem/ssh-zmodem-filter`**，字节匹配判定是否启动
  rzsz 会话；不是 rzsz 时才进入编码转换回传前端。见
  `docs/附录.WeTERM的SSH登录流程.md:55` 与 `docs/assets/A.3.backend.drawio`。
- 协议栈用 npm `zmodem.js@^0.1.10`（`yarn.lock:15922`），前端 xterm.js 只负责渲染。
- CHANGELOG 记录的真实坑位，正是我们设计要覆盖的：`rz` 路径/拖拽/终止、`zmodem` 异常二进制
  输出、多文件、跨服务器发错机器、citrix 远程目录、rzsz tracing 埋点。

**Agenterm 侧锚点**（本仓库）：

- PTY 读路径：`crates/warp_terminal/src/local_tty/event_loop.rs:199` `pty_read()`，
  `READ_BUFFER_SIZE = 0x4_0000`，`MAX_LOCKED_READ = 0x1_0000`；读完直接
  `state.parser.parse_bytes(terminal, &buf[..n], &mut terminal_response_sequences)`。
- 写路径：同一个 `State::write_list`；终端响应序列已经用
  `state.write_list.push_back(Cow::Owned(terminal_response_sequences))` 回写 PTY
  （`event_loop.rs:245`），这是 ZMODEM 回包可以复用的既有通道。
- 字节广播：`ChannelEventListener::send_pty_read_event()`（`event_listener.rs:87`）→
  `TerminalModel::on_finish_byte_processing()`（`app/src/terminal/model/terminal_model.rs:3326`）
  里调用；`async_broadcast` 容量 1024（`app/src/terminal/mod.rs:112`）。
- PTY 写入 API：`PtyController::write_bytes()`（`writeable_pty/pty_controller.rs:594`），
  经 `PtyIntent::WriteBytes` → `wire_up_pty_controller_with_surface`
  （`writeable_pty/terminal_manager_util.rs:71`）。**不要用 `typed_characters_on_terminal`，
  那会走编辑器/命令语义。**
- 应用事件上行：`event_proxy.send_app_event()` → `app/src/terminal/event.rs:28` `Event` →
  `ModelEventDispatcher` → `TerminalView::handle_terminal_event`（`view.rs:11942`）。
  现有 `RemoteServerReady` 等事件就是这套链路。
- 文件对话框：`ctx.open_save_file_picker(cb, SaveFilePickerConfiguration)` /
  `ctx.open_file_picker(cb, FilePickerConfiguration)`，见
  `crates/warpui_core/src/core/app.rs:4302/4315` 与 `platform/file_picker.rs:129`。
  现有调用示例：`app/src/editor/view/mod.rs:5053`、`app/src/ai/ai_document_view.rs:1050`。
- 通知：`ToastStack::handle(ctx)` + `DismissibleToast`（`view.rs:16663`）。
- 设置：沿用 `maybe_define_setting!` 模式，先例见 `app/src/terminal/warpify/settings.rs`。
- Feature flag：`crates/warp_features/src/lib.rs:8` `FeatureFlag` 枚举 + `app/src/features.rs:16`
  `enabled_features()`（Agenterm 侧只在这里开，不进 `DOGFOOD_FLAGS`）。
- 锁纪律：`TerminalModel` 的 `FairMutex`。`pty_read` 已有"拿不到锁就继续读、读满才阻塞"的策略；
  **新增逻辑不得在已持锁的调用栈里再 `model.lock()`**（AGENTS.md Terminal Model Locking）。

**依赖选型**：`zmodem2@0.7.2`（MIT OR Apache-2.0，`rust-version 1.85`，no_std，
heapless，caller-owned I/O，poll/submit 状态机）。本仓库 toolchain 1.92.0，
`deny.toml` / `about.toml` 的 license 白名单已包含 MIT 与 Apache-2.0，无需改白名单。
它只依赖 `bitflags 2.x`、`hex 0.4`、`thiserror 1.0`，三者均已在 `Cargo.lock`。
用户本机已有 homebrew `lrzsz`（`/opt/homebrew/bin/{sz,rz,lsz,lrz}`），可直接做端到端验收，
但**不作为运行时依赖**。

## Scope 与边界

- In scope:
  - 在 PTY 读路径新增 ZMODEM **下载探测与接管**（`sz` 方向）：识别 `ZRQINIT`/`ZFILE`
    后接管字节流，用 `zmodem2::Receiver` 收文件，写本地磁盘。
  - 新增 **上传入口**（`rz` 方向）：Command Palette 动作 + 可选右键菜单项，
    选本地文件后用 `zmodem2::Sender` 发送。
  - `ZmodemTransfer` 面板/状态视图：文件名、字节数、进度、取消按钮、完成/失败 toast。
    复用现有 `DismissibleToast` 与 `ssh_file_upload.rs` 的展示风格（不复用其 sftp 逻辑）。
  - 传输期间**抑制协议字节渲染**（不把二进制喷进 scrollback），并在终端留下
    一行人类可读的传输记录。
  - 本地设置：默认下载目录、是否启用 ZMODEM、上传默认目录（可选）。
  - 定向单元测试（探测器 + 状态机驱动）+ 本机 `sz`/`rz` 端到端验收。
- Out of scope:
  - 依赖或调用外部 `lrzsz` 二进制。
  - SFTP / scp / 拖拽上传（现有 `SshDragAndDrop` 走 sftp，本次不动）。
  - 修改 SSH 配置、远端 RC、跳板机策略。
  - 远端自动部署 Warp 组件；本方案是纯客户端实现，远端只需有 `sz`/`rz`。
  - XMODEM / YMODEM / Kermit。
  - 目录递归传输（`sz -r` 的目录展开语义）、断点续传之外的 ZMODEM 扩展
    （ZCHALLENGE 解密、ZCOMPRLZW 压缩）。
  - 自动上传触发（本次上传必须用户显式发起）。
- 假设（**若与预期不符请纠正，这三点直接决定实现范围**）：
  1. 下载走**自动探测**（WeTERM 同款 zmodemfilter）；上传走**显式入口**，
     因为 `rz` 需要远端先发 `ZRINIT`，客户端无法凭空触发。
  2. 下载落地用**原生保存对话框**（`open_save_file_picker`），默认目录为上次选择或
     `~/Downloads`；这是最可预测且已有多处先例的路径。
  3. 协议栈用 **`zmodem2` crate**，不自己实现 CRC/ZDLE/协商。
- 关键约束/不变量：
  - **不改共享 PTY 读路径的默认行为**：未探测到 ZMODEM 时，字节必须原样进入 ANSI 解析器，
    现有渲染/录制/共享会话链路零变化。
  - **不新增 `model.lock()` 调用点**：接管逻辑在 event loop 线程内完成，
    只通过 `event_listener` / `send_app_event` 上行，不在持锁路径里取锁。
  - **PTY 写入只走 `PtyController::write_bytes`**（即 `Message::Input`），
    不直接碰 `mio_channel`，避免绕开 `kill_buffer` 拆分和断连处理。
  - **不阻塞 event loop**：文件 I/O 在后台/主线程异步任务里做；协议状态机本身是
    纯内存 poll/submit，可以在 event loop 内驱动，但每个 poll 循环必须有字节/回合上限。
  - **传输可取消**：任何时候取消都必须能发 abort 序列并恢复普通渲染，
    不能把终端留在永久接管态。
  - 不支持/失败时**安全回退**：把字节交还给 ANSI 解析器（允许显示乱码），
    而不是吞掉输出或冻结终端。
  - 不记录文件内容、不记录完整命令行到日志。
- 需要重新获批的变化：新增第三方依赖（已在下方 Artifact 决策里显式列出并说明理由）、
  修改共享 event loop 的字节处理顺序、把 ZMODEM 探测扩展到非 SSH 本地会话。

## 成功标准

- [ ] 本地 shell 里跑 `sz <file>`，Agenterm 弹出保存对话框，保存后本地文件与远端
      `md5`/字节数一致；终端显示一行传输记录，scrollback 没有二进制乱码。
- [ ] 通过显式入口选本地文件，远端 `rz` 能收到同名同内容文件（用 homebrew `rz` 在本机验收）。
- [ ] 取消/中断（传输中取消、`Ctrl-C`、远端 abort）都能退出接管态，终端继续可用。
- [ ] 非 ZMODEM 输出（含二进制但非协议的流，例如 `cat /bin/ls`）行为与今天完全一致，
      现有终端回归测试无变化。
- [ ] 新增定向测试：探测器在真实 `sz` 前导字节上触发、在随机二进制上不触发；
      `Receiver`/`Sender` 状态机驱动在合成 wire 字节上走完一轮。
- [ ] `./script/format`、presubmit 的 Clippy 三段、相关 nextest 全部通过。
- [ ] 最终交付：变更摘要、验证证据（端到端截图/日志、测试命令输出）、
      依赖新增说明、以及明确列出的剩余风险（见下）。
- [ ] 最终 artifacts 反映真实状态：完成、跳过、阻塞和验证结果。

## Artifact 决策

- `spec.md`: **required** — 这是跨 `warp_terminal`（共享 PTY 读路径）、`app/src/terminal`
  （事件/UI/设置）、依赖新增的横切行为变化，且探测阈值、接管边界、失败回退都需要
  长期可查的设计记录。用 `spec-writing` 新建
  `docs/design-docs/terminal/zmodem-rzsz/spec.md`。
- `tasks.md`: **required** — 涉及依赖接入、event loop 改造、新的 model/view、设置、测试，
  需要拆分给 implementer 并写明 lock/事件链路的 context。
- 依赖新增：`zmodem2 = "0.7.2"`（MIT OR Apache-2.0）。理由：ZMODEM 的 CRC16/CRC32、
  ZDLE 转义、ZRINIT 能力协商、ZRPOS 断点续传都属于易错细节，且本功能是纯增量特性、
  不值得自建并维护一套协议栈；license 已在 `deny.toml`/`about.toml` 白名单内。
  **此项需要用户明确批准**（新增第三方依赖）。

## 质量门禁

- [ ] Gate: Plan Approval — 批准这个 `plan.md` 后才能开始执行。
- [ ] Gate: Spec Review — 涉及 `spec.md`，由 `spec-writing` 在 spec 完成时 present。
- [ ] Gate: Code Review — 涉及共享 PTY 读路径与新增依赖，用 `code-review` Standard 模式。
- [ ] Gate: Final Review — 批准最终结果、验证证据和提交准备状态。

## 步骤

1. [x] 调研 WeTERM 的 zmodemfilter 架构与 Agenterm 的 PTY 读/写/事件链路，确定锚点。
2. [ ] 调用 `spec-writing` 起草 `docs/design-docs/terminal/zmodem-rzsz/spec.md`
      → Gate: Spec Review。重点写清：探测触发条件与误判边界、接管期间的字节所有权、
      失败回退、取消语义、与 SSH Warpify / 共享会话 / block bootstrap 的交互、
      依赖与 license 理由、测试策略。
3. [ ] 调用 `task-planning` 拆分 `tasks.md`。
4. [ ] 完成 `tasks.md` 中的所有任务。
5. [ ] 本机端到端验收：用 homebrew `lrzsz` 在真实 shell 里跑 `sz` / `rz`，
      覆盖成功、取消、非协议二进制、多文件。
6. [ ] 定向回归 + `./script/format` + presubmit Clippy。
7. [ ] 代码评审 → 调用 `code-review` → Gate: Code Review。
8. [ ] 准备最终交接 → Gate: Final Review（未获明确要求前不提交、不推送）。

## 已知风险与需人工确认项

- **误判风险**：ZMODEM 前导是 `**\x18B...`（ZHEX 头）这类短字节序列。探测器必须要求
  完整合法 header + CRC 通过才进入接管，且加一次性超时；否则会把正常二进制输出
  误吞成乱码。这是 spec 必须钉死的边界。
- **编码与 8-bit 通道**：ZMODEM 需要 8-bit clean 通道。如果远端/本地 termios 或
  Warpify bootstrap 对字节做了转义，协议会失败。需要在 spec 里确认当前 PTY 配置
  是否满足，并准备失败回退。
- **远端必须有 sz/rz**：纯客户端实现不能凭空提供能力；验收依赖远端已装 `lrzsz`。
  本方案不安装、不修改远端。
- **跨会话串行**：同一时刻只允许一个 ZMODEM 会话；WeTERM CHANGELOG 里的
  "跨服务器传输发错机器"就是这类坑，spec 需要明确 session 绑定。
