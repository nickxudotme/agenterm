# 代码实现任务清单

> 基于 `.agent/warp-drive-only-tool-panel/plan.md` 与 `docs/design-docs/workspace/local-warp-drive/spec.md` 生成
> 任务总数：11
> 状态标记：[ ] 待办, [~] 进行中, [x] 完成, [-] 跳过/废弃, [!] 阻塞

## 当前状态

- 当前任务：完成；等待真机交互验证（打开 Tool Panel → 新建 workflow → 运行）
- 阻塞任务：无
- 下一个可执行任务：任务 1
- 上次同步：2026-09-12，方向调整 + 云通道整删完成
- 方向调整：不再自建 Drive 面板，改用仓库现成的 `DrivePanel`/`DriveIndex`（与 zap 同源，含搜索与
  workflow 编辑弹窗）。原因是 zap/agenterm 的 Drive UI 本已存在，且其下层 `UpdateManager` 已本地化，
  UI 天然可用。已删除 `app/src/drive/local_panel.rs`、`app/src/workflows/local_drive_model.rs`。
- 保留：YAML 本地存储模型 `app/src/workflows/local_drive_store.rs`（14 个单测通过），作为本地
  workflow 文件的读取/写入与刷新基础。
- 云通道整删（按 zap 做法）：删除 `sync_queue` 及三个云二进制；`update_object`/`create_object` 走
  本地分支（内存 + SQLite，不进同步队列）；`SerializedModel` import 指向 `cloud_objects`。
- 测试：mini(M4) 全量 6489 通过 / 15 失败；与基线(未改动,13 失败)相比仅 2 个差异，且单独跑均通过
  （既有 flaky/串扰，非本次引入）。
- 新增证据测试 `test_local_workflow_persists_without_server_round_trip`：workflow 创建→更新→内存
  可读→SQLite 落盘，且 mock 断言零云调用。已通过。
- 构建：`./script/run` 成功产出 `target/debug/bundle/osx/Agenterm.app`；启动后日志无 panic、
  无云初始加载阻塞。
- 本地化判断点（10 处 `ChannelState::is_local_warp_drive()`）：Tool Panel 只显示 Warp Drive、
  不要求账号、`update_object`/`create_object` 走本地分支、DriveIndex 不等云加载、离线视为可用、
  本地模式隐藏无意义的 Retry 菜单项。

## 依赖关系与并行化

```
任务 1 (本地存储模型)
   ├──> 任务 2 (文件监听/刷新) ──┐
   └─────────────────────────────┼──> 任务 4 (本地 Drive 面板) ──> 任务 6 (编辑器) ──> 任务 7 (运行桥接)
任务 3 (通道 Tool Panel 策略) ───┘                                                          │
                                                                                            v
                                                          任务 5 (Workspace 接线) <──────────┘
                                                                    │
                        ┌───────────────────────────────────────────┴────────────────────┐
                        v                                                                v
             任务 8 (测试与 fixture)                                        任务 9 (离线/故障注入测试)
                        └──────────────────────────┬───────────────────────────────────┘
                                                   v
                                        任务 10 (格式/Clippy/build)
                                                   │
                                                   v
                                        任务 11 (GUI smoke test)
```

### 并行组

- 起始可并行：任务 1 与任务 3 互不依赖，可同时派发。
- 任务 2 依赖任务 1；任务 4 依赖任务 1；任务 2 与任务 4 在任务 1 完成后可并行。
- 任务 5、6、7 依赖任务 3 与任务 4；其中任务 7 可与任务 6 并行开发（都以任务 4 的面板事件为契约）。
- 任务 8、9 在任务 5/6/7 完成后可并行。
- 任务 10、11 顺序执行，且必须在 8/9 之后。

## 任务列表

### 任务 1：[x] 实现本地 Workflow 存储模型（扫描、身份、快照、受管 CRUD）

- **文件**：`app/src/workflows/local_drive_store.rs`（新建）、`app/src/workflows/mod.rs`（修改：注册模块）
- **依赖**：无
- **来源映射**：`spec.md` §4.4「本地 Workflow 存储模型」、§4.5.1、§4.5.2、§6.6
- **说明**：新增 `LocalDriveStore` 单例模型，作为 Agenterm 本地 Warp Drive 的唯一数据源与唯一写入入口。它负责：

  - 初始扫描 Agenterm 数据目录下 `workflows` 目录（递归）中的 YAML，为**每个有效文档**构造一条记录，记录含本地身份（来源路径 + 文档序号）、`Workflow` 内容、受管属性、文件内容版本（如 mtime+len 或内容哈希，实现时选稳定且廉价的一种）。
  - 分类：仅当文件位于 `workflows/drive`、文件名符合 Agenterm 生成规则（不含用户输入，例如 `wf-<随机稳定 id>.yaml`）、且内容**恰好一个**普通命令 Workflow 文档时为受管对象；其余为外部管理（只读，可显示可运行）。
  - 提供受管 CRUD：create（生成随机文件名，写 `workflows/drive`）、update（按本地身份重写单文档文件）、delete（仅受管单文档文件）。所有写入先写同目录临时文件、落盘后原子替换；目录/文件权限收紧为当前用户专用（0600/0700），替换不得放宽权限；路径必须规范化且限制在 Workflow 根目录内，拒绝 `..` 与符号链接逃逸。
  - 更新/删除前比较编辑开始时记录的内容版本与当前文件版本，不一致则拒绝并返回冲突错误，不覆盖。
  - 快照对外暴露：条目列表、`generation`（单调递增）、按文件的错误列表；旧扫描结果不得覆盖较新 generation 状态。
  - 日志区分扫描/解析/创建/更新/删除/冲突，记录路径与错误类别，**绝不记录命令正文或参数值**。

- **context**：
  - `app/src/workflows/local_workflows.rs`：`LocalWorkflows`、`workflows_dir()`、`UseCache`，现有本地 Workflow 加载与缓存方式。
  - `app/src/user_config/native.rs:load_workflows()`、`app/src/user_config/util.rs:for_each_dir_entry()`、`parse_multi_workflow_dir_entry()`：递归读取与多文档 YAML 解析，复用其读取语义。
  - `app/src/user_config/mod.rs:194`：`warp_core::paths::data_dir()`，Agenterm 独立数据目录来源。
  - `crates/cloud_object_models/src/workflow.rs`：`Workflow` 枚举（`Command` / `AgentMode`）、`Argument`、序列化形状；`is_command_workflow()` 用于过滤非命令 Workflow。
  - 下游消费方：`app/src/user_config/native.rs:59,108` 现有 workflow 加载/刷新订阅（任务 2 接入）。
- **验收标准**：
  - [ ] `cargo check -p warp` 通过（模块被引用或至少被编译进 crate）。
  - [ ] 单元测试覆盖：单文档/多文档 YAML 读取、受管与外部分类、原子创建/更新/删除、外部修改冲突拒绝、非法路径被拒、解析失败处理（首次失败只报错误、已加载后失败保留错误快照且禁止运行/编辑/删除）、权限设置。
  - [ ] 单元测试断言：写入失败后磁盘仍保留最后有效版本；临时文件不进入快照。
  - [ ] 单元测试断言：日志输出不含命令正文（可用捕获日志的字符串断言）。

### 任务 2：[~] 接入文件监听与刷新收敛

- **文件**：`app/src/workflows/local_drive_store.rs`（修改）、`app/src/user_config/native.rs`（修改）
- **依赖**：任务 1
- **来源映射**：`spec.md` §4.5.3、§6.2、§6.3
- **说明**：把 `LocalDriveStore` 接到现有 workflows 目录文件监听链路：启动时异步扫描并发布快照；文件事件先做短暂合并（debounce）再按最终目录状态生成一次刷新；事件与本应用自身写入事件合并后收敛到最终磁盘状态（rename-save 的中间删除/创建事件按最终状态处理）。刷新结果携带递增 generation，旧结果丢弃。文件在合并刷新后确认不存在则移除条目；此前已存在且暂时不可读/不可解析的文件保留最后有效快照并标记错误。文件恢复有效后替换快照并清除错误。扫描与写入都在后台线程，UI 只接收完整结果，且不持有终端模型锁。
- **context**：
  - `app/src/user_config/native.rs:59,108-111`：现有 `update_touches_dir()` + 异步 `load_workflows()` 刷新范式，`RepositoryUpdate` 事件来源。
  - `app/src/user_config/util.rs`：`repository_update_touches_prefix()`、`repository_update_touches_path()`。
  - 上游：`app/src/workflows/local_drive_store.rs`（任务 1 的存储模型 API）。
  - 下游：`app/src/user_config/WarpConfig` 的 workflow 更新事件订阅者（任务 4/5 的面板消费）。
- **验收标准**：
  - [ ] 编译通过。
  - [ ] 单元测试覆盖：外部修改、外部删除、rename-save、暂时不可读、非法 YAML、恢复有效；断言 generation 单调且旧结果不覆盖新结果。
  - [ ] 单元测试断言：合并刷新在 rename-save 场景下不产生条目丢失。

### 任务 3：[x] Agenterm 通道的 Tool Panel 唯一视图策略与按钮恢复

- **文件**：`app/src/workspace/view.rs`（修改）、`app/src/workspace/view/left_panel.rs`（修改）、`app/src/drive/settings.rs`（修改）
- **依赖**：无
- **来源映射**：`spec.md` §4.4「Workspace 工具面板策略」、§3.1.1-3.1.3、§4.3
- **说明**：在 Agenterm（OSS）通道：

  - `Workspace::compute_left_panel_views()` 直接返回 `vec![ToolPanelView::WarpDrive]`；其他通道维持现有动态策略不变。
  - `ToolPanelView::WarpDrive` 在 Agenterm 通道的 `availability()` 返回 `Available`（不因匿名/未登录而变成 `RequiresAccount`），确保不出现登录门槛与 Sign in 按钮；`app/src/workspace/view.rs:21096` 处 `Channel::Oss` 对 header toolbar 按钮的裁剪按本地面板需求复核处理，使 Tool Panel 按钮可见可点。
  - 单视图下标题栏 tooltip 文案保持“Warp Drive”（现有 `left_panel_views.len() <= 1` 分支已覆盖），确认按钮渲染路径不再被 OSS 分支隐藏。
  - 不恢复 `Listener` 的 OSS 初始化，不解除 RootView 对 OSS 登录流程的跳过。

- **context**：
  - `app/src/workspace/view.rs:23557 compute_left_panel_views()`、`view.rs:20400-20490` 与 `20455-20490` 的 tooltip/按钮渲染、`view.rs:21096` OSS header 裁剪、`view.rs:8663` OSS 初始页。
  - `app/src/workspace/view/left_panel.rs:96-127 ToolPanelView::availability()`、`left_panel.rs:252-330 render_unavailable_panel()`（确认 Agenterm 不会走进 RequiresAccount 分支）、`left_panel.rs:499 update_available_views()`、`left_panel.rs:542 create_toolbelt_button_config()`。
  - `app/src/drive/settings.rs`：`WarpDriveSettings::is_warp_drive_available()` / `is_warp_drive_enabled()`，现有匿名/登出返回 false 的逻辑。
  - 下游：`app/src/workspace/view_tests.rs`（现有 left panel 视图测试，需同步更新）。
- **验收标准**：
  - [ ] 编译通过。
  - [ ] 单元测试断言：Agenterm 通道 `compute_left_panel_views()` 恰好返回 `[WarpDrive]`；非 Agenterm 通道行为不变（现有测试通过）。
  - [ ] 单元测试断言：Agenterm 通道 WarpDrive 的 availability 为 `Available`，且不会渲染 Sign in 按钮。
  - [ ] 现有 `app/src/workspace/view_tests.rs` 相关用例全部通过。

### 任务 4：[~] 本地 Warp Drive 面板（列表、搜索、空状态、CRUD 入口）

- **文件**：`app/src/drive/local_panel.rs`（新建）、`app/src/drive/mod.rs`（修改：注册模块）
- **依赖**：任务 1
- **来源映射**：`spec.md` §4.4「本地 Warp Drive 面板」、§3.1.3-3.1.11、§4.3「用户可见接口」、§6.5
- **说明**：新建 `LocalDrivePanel` 视图，只消费 `LocalDriveStore` 快照，不引用账号、云对象、`Owner`、`SyncId` 或同步状态。功能：

  - 列表仅显示本地普通命令 Workflow；按名称或命令内容过滤。
  - 标题区提供新建入口；列表项提供运行、编辑、删除入口（外部管理条目的编辑/删除禁用并标记为“外部管理”）。
  - 空状态显示本地说明与新建入口，不显示在线文档、登录或团队引导。
  - 删除需确认，取消不改变文件或列表。
  - 错误快照条目禁止运行/编辑/删除并显示可见错误原因；用户可修复的问题转成 UI 可见错误而非只写日志。
  - 订阅快照变化，保持当前选择落在仍存在的本地身份上；具备键盘导航与稳定的 accessibility 名称（面板、列表、编辑操作、只读标记、错误状态）。

- **context**：
  - `app/src/drive/panel.rs`：`DrivePanel` 与 `DrivePanelEvent` 的结构与 Workspace 交互方式（作为形状参考，**不要**复用其云事件）。
  - `app/src/workflows/local_drive_store.rs`（任务 1）：快照、条目、受管属性、错误状态 API。
  - `app/src/workflows/categories.rs`：`CategoriesView` 列表渲染、过滤、键盘导航与 accessibility 命名可参考的实现。
  - 下游：`app/src/workspace/view/left_panel.rs:219,1295,1363`（warp_drive_view 挂载点，任务 5 接线）。
- **验收标准**：
  - [ ] 编译通过。
  - [ ] 组件/单元测试：未登录状态下列表可用、空状态文案本地、外部条目编辑/删除禁用、错误条目三项操作禁用、删除确认取消无副作用、搜索按名称与命令命中。
  - [ ] accessibility 名称在列表、新建/编辑/删除操作、只读标记、错误状态上均有稳定取值（测试断言）。

### 任务 5：[x] 把本地 Drive 面板接入 LeftPanel 与 Workspace

- **文件**：`app/src/workspace/view/left_panel.rs`（修改）、`app/src/workspace/view.rs`（修改）
- **依赖**：任务 3、任务 4
- **来源映射**：`spec.md` §4.4、§4.5.4、§3.1.9、§6.3
- **说明**：Agenterm 通道下 `LeftPanelView` 的 Warp Drive 视图指向 `LocalDrivePanel`，其他通道继续使用云端 `DrivePanel`。Workspace 订阅本地面板事件并路由到运行桥接（任务 7）；面板打开/使用不构造云监听、不发起网络请求。确保面板打开时焦点行为与现有 WarpDrive 分支一致，且 `update_available_views()` 在唯一视图场景下不产生多余 toolbelt 切换。
- **context**：
  - `app/src/workspace/view/left_panel.rs:219 warp_drive_view`、`left_panel.rs:354,469`、`left_panel.rs:866,1161,1295,1363` 各 WarpDrive 分支。
  - `app/src/workspace/view.rs:17636 handle_warp_drive_event()`、`:17940 run_workflow_in_active_input()`。
  - `app/src/drive/local_panel.rs`（任务 4）的事件定义。
- **验收标准**：
  - [ ] 编译通过（含非 Agenterm 通道编译路径）。
  - [ ] 单元测试：Agenterm 通道 Warp Drive 视图为本地面板（可用句柄类型或显式的通道分支断言），非 Agenterm 通道仍为云端面板。
  - [ ] 断言：打开面板路径不触发云客户端初始化（任务 9 的离线替身复用）。

### 任务 6：[ ] 本地 Workflow 编辑器（新建/编辑，保存走本地存储）

- **文件**：`app/src/workflows/workflow_view.rs`（修改）或 `app/src/drive/local_workflow_editor.rs`（新建，实现者选择侵入性更小的方案）
- **依赖**：任务 4
- **来源映射**：`spec.md` §4.4「本地 Workflow 编辑器」、§3.1.5-3.1.8、§4.3「错误语义」、§6.2
- **说明**：提供本地 Workflow 的创建与编辑表单：名称、命令、可选说明、标签、文本参数（本期仅文本）。保存目标为 `LocalDriveStore`，不创建云对象标识、owner、folder 或同步任务。校验失败保留编辑内容并指出字段问题；编辑期间若发生应用已观察到的外部修改，不覆盖新内容并提示重新加载后重试；保存/删除失败保留当前内存快照与磁盘最后有效版本。
- **context**：
  - `app/src/workflows/workflow_view.rs`：`WorkflowView`、`WorkflowViewMode`、`WorkflowAction::RunWorkflow`、编辑态与可编辑性判断（注意 `WorkflowViewMode::supported_edit_mode()` 依赖云 `object_editability`，本地路径必须绕开）。
  - `app/src/drive/workflows/modal.rs`：现有云端 Workflow 创建/编辑弹窗字段与校验（形状参考，不复用云保存）。
  - `crates/cloud_object_models/src/workflow.rs`：`Argument`、`ArgumentType`，文本参数构造方式。
  - `app/src/workflows/local_drive_store.rs`（任务 1）：create/update API 与冲突错误。
- **验收标准**：
  - [ ] 编译通过。
  - [ ] 单元测试：新建写入受管目录且文件名不含用户输入/不含名称；编辑保留文件名与本地身份；校验失败保留输入；外部修改冲突下保存被拒且提示重新加载；保存失败不破坏最后有效版本。
  - [ ] 断言：保存路径不产生云对象请求（任务 9 离线替身复用）。

### 任务 7：[x] Workflow 运行桥接（送入当前终端，不自动执行）

- **文件**：`app/src/workspace/view.rs`（修改）、`app/src/drive/local_panel.rs`（修改）
- **依赖**：任务 4
- **来源映射**：`spec.md` §4.4「Workflow 运行桥接」、§4.5.4、§3.1.9
- **说明**：本地面板发出“运行本地 Workflow”事件后，Workspace 将其路由到当前活动终端的既有 Workflow 参数/输入链路：有参数时先显示参数输入，无参数时把命令放入输入框；不自动执行。没有活动终端或条目处于错误快照时，面板禁用运行并显示原因，绝不隐式创建或执行终端。该路径不写入本地存储、不创建后台执行。命令进入终端前不持有终端模型锁跨越文件 I/O 或 Workflow UI 更新。
- **context**：
  - `app/src/workspace/view.rs:17940 run_workflow_in_active_input()`、`:17763 run_cloud_workflow_in_active_input()`、`:17790 focus_terminal_input()`。
  - `app/src/terminal/input.rs`：`show_workflows_info_box_on_workflow_selection()`，参数填写与命令注入链路。
  - `app/src/workflows/info_box.rs`：参数替换与 `command_with_replaced_arguments`。
  - `app/src/workflows/mod.rs:126`：`ContextFlag::RunWorkflow` 判定。
  - `app/src/workflows/workflow_enum.rs`：`WorkflowType` 及 `as_workflow()`（本地 Workflow 需要能被这条链路接受；必要时扩展枚举或转换，但不得引入云对象 ID）。
- **验收标准**：
  - [ ] 编译通过。
  - [ ] 测试覆盖：无参数 Workflow、带文本参数 Workflow、无活动终端时禁用并提示、ssh warpified 终端场景；断言命令进入输入框且**未**被自动执行。
  - [ ] 断言：运行路径不写入本地存储文件（除既有编辑保存外）。

### 任务 8：[ ] 兼容性与规模测试及 fixtures

- **文件**：`crates/integration/tests/data/test_workflow.yaml`（复用/补充）、`app/src/workflows/local_drive_store_tests.rs`、`app/src/drive/local_panel_tests.rs`（新建或修改）
- **依赖**：任务 5、任务 6、任务 7
- **来源映射**：`spec.md` §7「兼容性测试」「规模测试」「可访问性测试」、§3.2、§6.4
- **说明**：为本地 Warp Drive 补齐测试与 fixtures：

  - 兼容性：使用现有多文档 Workflow fixture，验证可读取、可运行，编辑/删除禁用，且未知字段、注释、字段顺序、文本格式与符号链接目标不被改写。
  - 规模：生成 1,000 个 Workflow、YAML 总量约 10 MiB 的 fixture（测试内生成，不入仓超大文件），验证文件 I/O 不在 UI 线程执行、面板先渲染内存快照。
  - 可访问性：补齐面板、列表、编辑操作、只读标记、错误状态的 accessibility 名称断言（与任务 4 配合）。

- **context**：
  - `crates/integration/tests/data/test_workflow.yaml`：现有多文档 fixture。
  - `app/src/workflows/local_workflows_tests.rs`：现有测试文件命名与放置约定（`AGENTS.md` 要求 `*_tests.rs` + `#[path]` 引入）。
  - `app/src/drive/index_tests.rs`：云端 Drive 测试组织方式（仅参考结构，不复制云依赖）。
- **验收标准**：
  - [ ] `cargo nextest run -p warp --no-fail-fast`（或仓库等价定向命令）下新增测试全部通过。
  - [ ] 兼容性断言：外部文件不被应用重写（逐字节比较保存前后）。
  - [ ] 规模测试在后台线程完成 I/O，且面板在扫描完成前即可渲染已有快照。

### 任务 9：[ ] 离线与故障注入测试

- **文件**：`app/src/workflows/local_drive_store_tests.rs`、`app/src/drive/local_panel_tests.rs`（新建或修改）
- **依赖**：任务 5、任务 6、任务 7
- **来源映射**：`spec.md` §7「离线测试」「故障注入测试」「外部变更测试」「权限测试」、§3.2「离线性」、§6.2、§6.6
- **说明**：安装“云客户端一旦使用即失败”的测试替身，覆盖初始化、空状态、CRUD、刷新、运行、冲突和错误展示，证明没有网络调用或账号交互。故障注入覆盖：临时写入失败、落盘失败、原子替换失败、删除失败、遗留临时文件后重启恢复、旧扫描结果不覆盖新写入结果。权限测试覆盖受管目录、临时文件、最终文件权限，并确认替换不放宽权限。
- **context**：
  - `app/src/drive/settings.rs`：`WarpDriveSettings` 与可用性判定，离线替身需要断言其不被调用为网络路径。
  - `app/src/workflows/local_drive_store.rs`（任务 1、2）的可失败 I/O 接缝（实现者需为测试留出注入点，例如将 fs 操作抽象为可替换的 trait 或函数指针）。
  - `AGENTS.md`：测试文件命名与 `#[path]` 约定。
- **验收标准**：
  - [ ] 新增离线测试全部通过；任何云/网络调用在测试中直接 panic 或返回错误，测试仍通过。
  - [ ] 故障注入测试断言：失败后内存快照和磁盘最后有效版本均未被破坏。
  - [ ] 权限测试断言：目录 0700、文件 0600（或等价的当前用户专用权限），替换后权限未放宽。

### 任务 10：[ ] 格式、Clippy 与构建

- **文件**：全仓（必要时）
- **依赖**：任务 8、任务 9
- **来源映射**：`plan.md` 成功标准「`./script/format --check`、Clippy、定向测试和 GUI build/signing 通过」
- **说明**：运行 `./script/format`（或 `--check`）、`cargo clippy --workspace --all-targets --all-features --tests -- -D warnings`（以 `./script/presubmit` 中定义为准）、定向 nextest 与 GUI build/signing，修复所有问题。禁止为通过检查而放宽 lint 或新增 `allow` 除非有充分理由并写注释。
- **context**：
  - `./script/presubmit`、`./script/format`：仓库定义的检查入口。
  - `AGENTS.md`：格式化 `max_width` 为 100，Clippy 要求 PR 前全绿。
- **验收标准**：
  - [ ] `./script/format` 无 diff。
  - [ ] Clippy 无 warning（`-D warnings`）。
  - [ ] 定向 nextest 通过；GUI build/signing 成功。

### 任务 11：[ ] 真实 GUI smoke test

- **文件**：无代码改动；产出验证记录到 `.agent/warp-drive-only-tool-panel/activity.md`
- **依赖**：任务 10
- **来源映射**：`plan.md` 成功标准「完成真实 GUI smoke test」
- **说明**：启动构建签名后的 Agenterm，完成：打开 Tool Panel（未登录/离线）、只显示 Warp Drive、创建 Workflow、运行到当前终端、编辑保存、重启后内容保持一致、删除生效；检查日志中无登录或云请求。把结果、证据与剩余风险写入 `activity.md`。
- **context**：
  - `AGENTS.md`：`cargo run` / `./script/run` 启动 GUI。
  - 本任务所有新增代码与日志埋点。
- **验收标准**：
  - [ ] 上述六项交互全部成功并有截图或日志证据。
  - [ ] 日志中无登录流程、无云 API 请求、无 Firebase 初始化。
  - [ ] `activity.md` 记录真实状态（完成/跳过/阻塞/风险）。

## 来源覆盖映射

| 来源 | 章节/条目 | 覆盖任务 |
| --- | --- | --- |
| `plan.md` 成功标准 | Tool Panel 按钮可操作 | 任务 3、5 |
| `plan.md` 成功标准 | 只显示 Warp Drive 且只含本地命令 Workflow | 任务 3、4 |
| `plan.md` 成功标准 | 本地 CRUD 与持久化、文件监听刷新 | 任务 1、2、6 |
| `plan.md` 成功标准 | 重启后恢复 | 任务 1、2、11 |
| `plan.md` 成功标准 | 参数填写并把命令送入终端 | 任务 7 |
| `plan.md` 成功标准 | 无登录 UI/账号门槛/云请求 | 任务 3、4、9 |
| `plan.md` 成功标准 | 新增与现有测试通过 | 任务 8、9、10 |
| `plan.md` 成功标准 | format/Clippy/build | 任务 10 |
| `plan.md` 成功标准 | GUI smoke test | 任务 11 |
| `spec.md` §3.1 | 功能性需求 1-11 | 任务 3（1-3）、4（3-8,11）、6（5-8）、7（9）、1/2（7-10） |
| `spec.md` §3.2 | 离线性/持久性/响应性/兼容性/隔离性/可访问性 | 任务 9（离线）、1（持久）、2/8（响应）、8（兼容）、1（隔离）、4/8（可访问） |
| `spec.md` §4.3 | 用户可见接口与错误语义 | 任务 4、6 |
| `spec.md` §4.4 | 五个组件设计 | 任务 3、1、4、6、7 |
| `spec.md` §4.5.1 | 本地身份与文件映射 | 任务 1 |
| `spec.md` §4.5.2 | 安全写入 | 任务 1、6 |
| `spec.md` §4.5.3 | 列表刷新 | 任务 2 |
| `spec.md` §4.5.4 | 运行 | 任务 7 |
| `spec.md` §6.2/§6.3 | 故障恢复与并发一致性 | 任务 1、2、9 |
| `spec.md` §6.5/§6.6 | 可观测性与安全性 | 任务 1、9 |
| `spec.md` §7 | 全部测试类别 | 任务 8、9、11 |
