# 修复 SSH Warpify 会话里 "Filter block output" 漏斗按钮不显示

## 目标

在 Warpify 后的 SSH 远端会话里，鼠标悬停命令 block 时，block 工具条上的
"Filter block output"（漏斗）按钮恢复显示，且过滤功能可用。

## 根因（已用回归测试验证）

`app/src/terminal/view.rs:12186-12205`：`BlockCompleted` 处理里，当完成的 block 类型是
`BlockType::InBandCommand` 时，跳过为 `next_block_index` 创建 mouse state。

- `next_block_index` 是"下一个 block"，也就是用户的下一条命令 block。
- Warpified 远端会话（非 ssh-wrapper，即 Agenterm 的 guarded SSH 流程）走
  `command_executor.rs:294-348` 的 `_` 分支，使用 `InBandCommandExecutor`：
  generator（远端目录列表、补全等）以 `Warp-Run-GeneratorCommand` 形式跑在同一条 PTY 上，
  于是用户命令之间会不断产生隐藏的 in-band block。
- 这些隐藏 block 完成时跳过填充，导致随后的用户 block 永远没有 mouse state。
- `view.rs:24912`（filter builder）与 `view.rs:24884`（bookmark builder）都用
  `mouse_states.get(block_index)?` 取 state，取不到就不产出元素 → 漏斗不渲染。
- 用户看到 ⋯（+90）和 save-as-workflow（+30）是因为它们在 `is_block_hovered` 下无条件 paint；
  `bookmark_elements` 实际上从未 paint，所以"书签还在"是误判，同因缺失。

验证：`terminal::view::ssh_warpify_tests::block_after_in_band_block_keeps_mouse_states`
（新增，先失败）断言 in-band block 之后的 block 应有 filter mouse state。

## 修复

`app/src/terminal/view.rs` 的 `ModelEvent::BlockCompleted` 分支：

1. 始终为 `next_block_index` 填充 label/bookmark/filter 三个 mouse state。
2. 完成的 block 是 in-band 且 in-band block 处于隐藏状态
   （`BlockVisibilitySettings::should_show_in_band_command_blocks == false`）时，
   删除该 block 自己那条 entry。这样隐藏 block 不占坑，map 规模仍受上游注释关心的有界性约束
   （mouse states 每帧 clone）。

## 成功标准

- [x] 新回归测试 `block_after_in_band_block_keeps_mouse_states` 通过（修复前先失败）。
- [x] `cargo test -p warp --lib` 全量：失败清单与修复前逐行一致（130 个失败为仓库既有失败）。
- [x] `./script/format` 通过。
- [x] `cargo clippy -p warp --all-targets --tests -- -D warnings` 通过。
- [x] 用户在真实 SSH Warpify 会话中确认：hover 命令 block 出现漏斗，输入关键字后输出被过滤。
      （2026-09-14 用户确认修复完成）

## 状态

done — Final Review 通过，已提交为一个 commit。

## 边界

- 不改渲染路径、不改 block 模型、不改 in-band generator 的执行方式。
- 本地（非 warpify）会话行为不变：本地 session 走 `LocalCommandExecutor`，没有 in-band block。
- 不启用/禁用任何 Warpify 或 SSH 功能开关；不做自动 Warpify。

## 质量门禁

- Plan Approval（本文件）
- Final Review（改完提交 diff + 验证证据）
