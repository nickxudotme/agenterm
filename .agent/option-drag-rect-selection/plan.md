# 按住 Option 拖拽做矩形（列）选择

## 状态：已放弃（2026-09-14 用户决定不改）

理由：确认现状是 Cmd+Option+拖拽已经可用（`from_mouse_event`，`FeatureFlag::RectSelection`
默认开启），够用；单 Option 只是"更贴近 iTerm2/WeTERM 习惯"，不值得为此改动。
本文件仅保留调研结论，未产生任何代码改动。

## 目标

在终端里按住 Option（macOS）拖拽鼠标即可做矩形选择，复制出来的也是按列裁剪的文本，与
WeTERM（xterm.js）的行为一致。

## 现状（证据）

- 矩形选择的能力已经全链路存在：`SelectionType::Rect`（`crates/warpui_core/src/text/mod.rs:23`）、
  模型层 `ExpandedSelectionRange::Rect`（`crates/warp_terminal/src/model/selection.rs:72`）、
  block 选择（`app/src/terminal/model/blocks/selection.rs:278-294`）、alt screen
  （`app/src/terminal/model/alt_screen.rs:171`）。
- `FeatureFlag::RectSelection` 已在 `app/Cargo.toml` 的 default features 里，且被加进
  `enabled_features()`（`app/src/features.rs:69`），所以运行时是开启的。
- 终端与 alt screen 在 mouse down 时都用 `SelectionType::from_mouse_event(modifiers, click_count)`
  （`block_list_element.rs:1576/1671`、`alt_screen_element.rs:275`）。
- 缺的是触发键：`from_mouse_event` 目前要求 macOS 上 **Cmd+Option**、其他平台 **Ctrl+Alt**
  （`crates/warpui_core/src/text/mod.rs:42-54`）。

## 改动

1. `crates/warpui_core/src/text/mod.rs`：`from_mouse_event` 在 macOS 上改为 **Option 单独按下**
   即 `SelectionType::Rect`（Cmd+Option 仍然成立，是它的超集）；非 macOS 保持 Ctrl+Alt，
   避免和 Linux/Windows 窗口管理器"Alt+拖拽移动窗口"冲突。
2. 不动 `block_list_element.rs:4569` 的 `is_selecting_blocks` 判断：它要求 cmd 或 shift，
   Option 单独按下不会触发整块多选，Cmd+Option 依旧被排除（矩形优先）。
3. 单测覆盖 `from_mouse_event`：Option → Rect；Option+Cmd → Rect；
   无修饰键 / 仅 Shift → 按 click_count 走 Simple/Semantic/Lines；双击、三击不受影响。

## 成功标准

- [ ] `from_mouse_event` 单测通过（含 macOS 分支）。
- [ ] `./script/format` 与 clippy 通过；全量测试失败清单与基线一致。
- [ ] 用户 `./script/run` 验证：终端里按住 Option 拖拽出现矩形选区，Cmd+C 复制出按列裁剪的
      文本；不按 Option 时行为不变（仍是普通流式选择，双击取词、三击取整行）。

## 边界

- 只改触发键，不新增设置项，不动选择/复制/渲染的实现。
- 不改非 macOS 平台的触发方式。
- 不处理"拖拽中途才按下/松开 Option"：与上游一致，选择类型在 mouse down 时确定。
- 全屏应用（alt screen）会一并生效，因为它复用同一个 helper。

## 质量门禁

- Plan Approval（本文件）
- Final Review（改完提交 diff + 验证证据）
