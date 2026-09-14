# 恢复 Agenterm 设置页的普通终端设置（只隐藏 AI/云/账号相关）

## 目标

设置里被整页移走的普通终端开关（copy on select 等）重新可见；AI / agent / 云 / 账号相关
的条目继续隐藏。

## 现状（证据）

- `settings_view/mod.rs:1407-1466`：OSS channel 下侧边栏只有 Appearance / Keybindings /
  Warpify / About。上游还有 **Features**（53 个 widget，copy on select、session 恢复、
  通知、全局热键、鼠标/滚动/focus reporting、Vim 模式、中键粘贴、右键行为、OSC 52、
  block 数上限等）和 **Privacy**。
- `settings_view/mod.rs:1469`：设置窗口的 OSS initial_page 白名单同样只有 3 页。
- `workspace/view.rs:8663`：以 settings pane 打开时的 OSS 白名单同样只有 3 页。
  三处必须一起放开，否则深层链接/命令面板跳过去仍被拦回 Appearance。
- `settings_view/keybindings.rs:58` `should_show_binding`：group 白名单漏了
  `AutoUpdate` 和 `Notifications`，这两个跟 AI/云无关。
- Features 页里需要隐藏的（AI/agent/共享会话/Drive）：
  AutosuggestionKeybindingHint、AutosuggestionIgnoreButton、AtContextMenuInTerminalMode、
  SlashCommandsInTerminalMode、OutlineCodebaseSymbolsForAtContextMenu、
  ShowTerminalInputMessageLine、ConfirmCloseSharedSession、WorkflowsInCommandSearch。
  （其中 3 个上游已被 FeatureFlag 关掉，这里显式隐藏以防开关变化。）
- Privacy 页里需要隐藏的：CloudConversationStorage（AI 会话存储）、
  DataManagement（"data management delete account"）。保留 Secret redaction、
  Crash reports、App analytics、Privacy policy。

## 改动

1. `settings_view/mod.rs`：把 OSS 导航抽成一个函数（便于测试），加入 Features、Privacy；
   两处 initial_page 白名单同步加入。
2. `workspace/view.rs:8663`：OSS 白名单加入 Features、Privacy。
3. `settings_view/keybindings.rs`：`should_show_binding` 的 group 白名单加入
   `AutoUpdate`、`Notifications`。
4. `settings_view/features_page.rs`：`build_page()` 末尾按 `widget_id()` 过滤掉上面 8 个
   widget（OSS channel 下）。用 `SettingsWidget::widget_id()` 集中过滤，避免在每个 push
   点散落判断。
5. `settings_view/privacy_page.rs`：同法过滤掉 CloudConversationStorage、DataManagement。

## 成功标准

- [x] 单测：OSS 导航包含 Features / Privacy，且不包含 Account / Teams / WarpDrive
      （`settings_view::tests::oss_settings_pages_*`）。
- [x] 单测：keybindings 过滤对 AutoUpdate / Notifications 放行，对 WarpAi / Workflow /
      Notebooks / Folders / EnvVarCollection 拦截（`settings_view::keybindings::tests::*`）。
- [x] `./script/format` 与 `cargo clippy -p warp --all-targets --tests -- -D warnings` 通过。
- [x] 全量 `cargo test -p warp --lib` 失败清单与基线一致（130 → 129，波动来自仓库既有
      flaky 测试，无新增失败）。
- [x] 用户 `./script/run` 打开设置：Features 页出现、copy on select 可切换生效，
      且看不到 AI 相关条目。（2026-09-14 用户确认）

## 状态

done — Final Review 通过，已提交为一个 commit。

## 边界

- 不恢复需要账号/云的页面：Account、Teams、BillingAndUsage、Agents 伞页（WarpAgent /
  AgentProfiles / AgentMCPServers / Knowledge / ThirdPartyCLIAgents）、Code、
  Cloud platform、Referrals、SharedBlocks、WarpDrive、Environments。
- 不改任何设置项默认值；不动 keybindings 里已有的 AI 名称过滤逻辑。
- 只动展示层，不改功能实现。

## 质量门禁

- Plan Approval（本文件）
- Final Review（改完提交 diff + 验证证据）
