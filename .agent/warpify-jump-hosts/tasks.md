# 代码实现任务清单

基于已批准 plan 与 restricted-ssh-warpify/spec.md；2 项任务。

## 当前状态

- 当前任务：完成。
- 阻塞：无。
- 下一步：如需无人确认自动接入，单独设计明确的 per-host opt-in；当前安全入口每次由用户确认。

## 依赖关系与并行化

- PG-A：任务 1 单 implementer，避免 view/状态/事件交叉修改。
- PG-B：任务 2 依赖 1，主 agent 执行验证。

## 任务列表

### 任务 1：[x] 恢复受保护的 SSH 主动接入提示

- 文件：app/src/terminal/ssh/util.rs 与 util_tests.rs；terminal/model/terminal_model.rs 及测试；terminal/view.rs、view/action.rs、view/block_banner/warpify.rs、view/use_agent_footer 相关文件；warpify/trigger_state.rs 及独立测试。仅按必要范围改动。
- 依赖：无。
- 来源：spec §3、§4、§6、§7。
- 说明：为普通 SSH 登录后的 Shell prompt 显示用户确认接入，绝不自动注入；绑定 Block/session 的资格并在点击再次校验，复用现有 bootstrap。未知 shell 使用现有检测，不硬编码远端为本地 shell。非 SSH 流程不变。认证和过早 Last login 不结束后续 prompt 观察，Pin+Token 明确识别。SSH 提示和“不再提示”采用正确主机语义；横幅/footer/快捷键统一校验，旧UI动作不可触发新会话。增加关联日志，无完整命令和输出。
- context：
  - ssh/util.rs check_ssh_login_state、parse_interactive_ssh_command；util_tests.rs。
  - terminal_model.rs start_notify_on_end_of_ssh_login、check_for_end_of_ssh_login、on_finish_byte_processing；model tests 的 event proxy。
  - view.rs AfterBlockStarted、handle_detected_end_of_ssh_login、trigger_subshell_bootstrap、on_user_block_completed、SourcedRcFileInSubshell、ShowSubshellBanner/TriggerSubshellBootstrap handlers。
  - view/block_banner/warpify.rs WarpifyBannerState::action/remember；view/use_agent_footer 的 Warpify 事件和 footer 呈现。
  - warpify/settings.rs enable_ssh_warpification/host denylist；trigger_state.rs 生命周期。
- 验收：
  - [x] 编译通过。
  - [x] 新增测试证明 authz/Last login/Pin+Token/分批最终prompt 判定，展示不写PTY。
  - [x] 资格测试覆盖退出、旧Block/session、重复点击、禁用/黑名单、Agent/viewer、已接入。
  - [x] 三入口统一校验，SSH拒绝更新host denylist，不改变非SSH动作。
  - [x] 不修改SSH/远端配置、无自动bootstrap调用；日志不包含认证/完整输出。

### 任务 2：[x] 验证、构建与交接

- 依赖：1。
- 来源：spec §7，plan成功标准。
- 类型：纯 verification，主 agent 执行。
- context：任务1 diff，现有 in-band/blocks/terminal_model/context_chips 测试，script/run，agenterm.log。
- 验收：
  - [x] 新增及相关既有回归通过，格式/diff检查通过。
  - [x] 正式 code-review 无剩余阻塞发现，主agent核实。
  - [x] GUI 构建签名成功，按既有授权重启供用户测试（PID 28789，codesign --verify --deep --strict 通过）。
  - [x] 用户实际219验证：不用手贴，点击后 Warpify 流程正常；用户确认整体行为正常。日志证明两次 offer shown -> user accepted，重复候选以 consumed 拒绝，远端 cwd 为 /data/home/nickhaoxu。

## 来源覆盖

验证证据：nextest run f46b4ae6-c8f2-4cd2-88f3-6319b9ac9ae2，149 passed / 0 failed（GUI lib，no-default-features，限定 SSH/Warpify/model/blocks/in-band/directory-fetcher 相关测试；不是全 workspace）；子 Shell UI 清理后定向回归 6/6。script/format、git diff --check、no-inline-test-module 检查以及仓库 presubmit 定义的 workspace/default-GUI/warp_completer 三段 Clippy 均通过。

| 来源 | 任务 |
| --- | --- |
| §3-4：入口、保护、生命周期与兼容 | 1 |
| §6：不新增协议、日志、资源和安全 | 1、2 |
| §7：回归与实际验收 | 1、2 |
| §8：非自动与残留风险报告 | 2 |
