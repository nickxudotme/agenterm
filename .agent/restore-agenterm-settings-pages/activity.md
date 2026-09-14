# Activity

### [2026-09-14] — Decision

用户反馈：设置里连 copy on select 这种普通开关都看不到，原本只想去掉 AI 相关。
排查发现不是逐项删开关，而是**整页被移出侧边栏**：`settings_view/mod.rs` 在 OSS channel 下
只保留 Appearance / Keybindings / Warpify / About，copy on select 属于被整页移除的 Features。
同样的 OSS 白名单还有另外两处（`settings_view/mod.rs` 的 initial_page、`workspace/view.rs`
opens settings pane 时的 initial_page），只改导航会让深层链接仍被拦回 Appearance。

### [2026-09-14] — Gate

Plan Approval 通过。用户定范围："把 AI 相关的都不展示就好"，并同意顺手修 keybindings 的
过度过滤（AutoUpdate / Notifications 两个 group）。

### [2026-09-14] — Decision

三处白名单统一到新函数 `oss_settings_pages()`，避免以后再出现"改了一处漏了两处"。
页面内条目用 `SettingsWidget::widget_id()`（默认即 `type_name::<Self>()`）集中过滤，
新增 `Category::without_widgets()`；不在每个 push 点散落判断。

### [2026-09-14] — Error

测试里用 `ChannelGuard` 切 OSS channel 后，全量跑时出现一次
`ai::mcp::file_based_manager::tests::tui_global_warp_servers_start_only_after_activation`
失败；单跑与后续两次全量均通过，判定为进程级 channel 的竞态（ChannelGuard 文档本身就警告过）。
改为把 `should_show_binding` 的 channel 作为参数传入
（`should_show_binding_in_channel`），测试不再动全局状态。

### [2026-09-14] — Decision

验证：`settings_view::*` 263 passed；新增 5 个测试通过；fmt / clippy 通过；
全量失败清单与基线一致（129~130 之间波动，均为仓库既有 flaky，无新增）。
运行时确认待用户 `./script/run` 打开设置页。

### [2026-09-14] — Gate

用户确认设置页恢复、copy on select 生效、无 AI 条目漏出，批准提交。Final Review 通过。

### [2026-09-14] — Decision

提交为一个 commit（`fix:` + `CHANGELOG-BUG-FIX:`）。未推送，未开 PR。
注意：本次只放回 Features 与 Privacy 两页；Account / Teams / Billing / Agents / Code /
Cloud platform / Referrals / SharedBlocks / WarpDrive 仍对 OSS 隐藏（需要账号或云）。
