# Plan：恢复仅含本地 Workflow 的 Tool Panel

## 目标

在 Agenterm 中恢复 Tool Panel，但只提供一个名为 Warp Drive 的本地 Workflow 面板。用户无需登录、无需联网，即可创建、查看、编辑、删除和运行 Workflow；任何操作都不得依赖 Warp 云端服务。

## 当前状态

- 状态：in_progress
- 当前步骤：执行 `tasks.md` 中的实现任务
- 上次同步：2026-09-11，`tasks.md` 已生成（11 个任务，含依赖 DAG 与来源覆盖映射）
- 下一步：并行执行任务 1（本地存储模型）与任务 3（通道 Tool Panel 策略）

## Scope 与边界

- In scope:
  - 在 Agenterm/OSS 通道恢复顶部 Tool Panel 按钮和左侧面板开关。
  - Agenterm 的 Tool Panel 可用视图固定为且仅为 `ToolPanelView::WarpDrive`。
  - Warp Drive 在 Agenterm 中进入本地模式，只展示普通命令 Workflow。
  - 复用现有 Workflow 数据模型、参数填写和向当前终端运行命令的能力。
  - 使用 Agenterm 自有本地目录中的 Workflow 文件持久化；优先复用现有 `~/.agenterm/workflows` YAML 加载、文件监听和刷新链路。
  - 提供本地 Workflow 的创建、编辑、删除以及重启后恢复。
  - 为 Tool Panel 可见性、仅 Warp Drive、本地 CRUD、持久化和运行链路增加回归测试。
- Out of scope:
  - Warp 登录、注册、Firebase、账号状态和任何登录提示。
  - Warp 云对象 API、RTC/cloud listener、同步队列上传、团队空间、分享和在线导入导出。
  - Notebook、Environment Variables、AI Facts、MCP Server、Agent Mode Workflow 等云端 Drive 对象。
  - Project Explorer、Global Search、Agent conversations、AI、Agent Mode 和 MCP client 面板。
  - 改变现有 SSH Warpify 行为。
- 假设：
  - UI 继续使用“Warp Drive”名称，但在 Agenterm 中其产品含义明确为本地 Workflow 库。
  - 本地 Workflow 使用已有 Workflow YAML 格式，数据放在 Agenterm 独立数据目录，不读取或修改正版 Warp 的用户数据。
  - “能使用 Workflow”包括创建、编辑、删除、填写参数并将命令送入当前终端，而不只是只读展示已有文件。
- 关键约束/不变量：
  - 打开面板、CRUD 和运行 Workflow 的正常路径不得发起网络请求。
  - 不恢复 `Listener` 的 OSS 初始化，不解除 RootView 对 OSS 登录流程的跳过。
  - 不通过伪造账号、个人 Drive owner 或云对象 ID 来复用云保存路径；本地对象必须保持本地语义。
  - 保留 Agenterm 独立 app ID、配置目录和现有 local-first 基线。
  - 参考 Zap 的显式 cloud-disabled 思路和本地 Drive 产品表达，但不照搬仍依赖账号、`Owner`、`SyncId`、`UpdateManager` 或云对象权限模型的路径。
- 需要重新获批的变化：
  - 任何登录、云同步或 Warp 服务端依赖。
  - Tool Panel 中增加 Warp Drive 以外的视图或对象类型。
  - 改为读取正版 Warp 的配置/数据库，或引入新的远程服务。

## 成功标准

- [ ] 未登录且网络不可用时，Agenterm 顶部显示可操作的 Tool Panel 按钮。
- [ ] 打开 Tool Panel 后只显示 Warp Drive，且 Warp Drive 内只显示本地普通命令 Workflow。
- [ ] 用户可创建、编辑和删除本地 Workflow，变更写入 Agenterm 自有本地目录并可被文件监听刷新。
- [ ] 重启 Agenterm 后，本地 Workflow 仍存在且内容一致。
- [ ] 用户可填写 Workflow 参数并把展开后的命令送入当前终端，现有终端/Block 行为不退化。
- [ ] 本地 Warp Drive 的核心路径没有登录 UI、账号门槛、分享/团队入口或云 API 请求。
- [ ] 新增测试覆盖 Agenterm 通道限定、唯一面板视图、本地 CRUD/持久化以及运行事件；相关现有测试继续通过。
- [ ] `./script/format --check`、仓库 presubmit 定义的 Clippy、定向测试和 GUI build/signing 通过。
- [ ] 完成真实 GUI smoke test：打开面板、创建 Workflow、运行、重启并复核持久化。
- [ ] 最终交付包含变更摘要、测试/build 证据、真实 GUI 验证结果和剩余风险。
- [ ] 最终 artifacts 反映真实状态：完成、跳过、阻塞和验证结果。

## Artifact 决策

- `spec.md`: required — 在 `docs/design-docs/workspace/local-warp-drive/spec.md` 记录跨 Tool Panel、Drive UI、Workflow 数据源和本地持久化的显著行为变化，明确本地模式、UI 裁剪和零云请求设计。
- `tasks.md`: required — 实现预计涉及 workspace/left panel、drive/workflow UI、user config 持久化和多组测试，需要拆分实现任务及依赖。

## 质量门禁

- [x] Gate: Plan Approval — 用户于 2026-09-11 回复“go”批准。
- [x] Gate: Spec Review — 用户授权自主推进；完整 spec 经独立 `spec-review` 全章节 PASS。
- [ ] Gate: Code Review — 实现和验证完成后调用 `code-review`。
- [ ] Gate: Final Review — 批准最终结果、验证证据和提交准备状态。

## 步骤

1. [x] 调研 Agenterm 对 Tool Panel、登录、云监听和 Warp Drive 的现有裁剪点。
2. [x] 确认本地 Workflow 已有 YAML 加载、Agenterm 独立目录、文件监听、参数展开和运行模型可复用。
3. [x] 调研 Zap：采用其显式 cloud-disabled 和本地 Drive 文案作为参考；确认其当前 Drive 仍残留账号门槛及云对象模型，因此只借鉴边界设计，不直接移植整套实现。
4. [x] 调用 `spec-writing` 起草并评审本地 Warp Drive `spec.md` → Gate: Spec Review。
5. [x] 调用 `task-planning` 将已批准 spec 拆分为 `tasks.md`。
6. [ ] 完成 `tasks.md` 中的所有任务。
7. [ ] 运行新增/相关回归测试、格式检查、Clippy 和 GUI build/signing。
8. [ ] 启动 Agenterm，完成本地 Workflow CRUD、运行和重启持久化 smoke test。
9. [ ] 调用 `code-review` 并处理成立的问题 → Gate: Code Review。
10. [ ] 同步 artifacts，准备最终 diff 与验证摘要 → Gate: Final Review。
