# Plan：219 Warpify 验证与菜单修复

## 2026-09-11 自动接入设置范围修订（暂缓）

### 目标与当前状态

- 目标：在已经验证的一键 SSH Warpify 基础上增加全局“自动 Warpify SSH 会话”设置；开启后，在原本会展示确认提示的同一安全资格点自动接入，不再要求点击。
- 状态：deferred；用户决定今天先保留已验证的一键手动接入版本，自动设置未获 Plan Approval、未进入 spec 修订或实现。后续恢复时从本节重新确认。
- 已验证现状：当前手动入口两次实际 219 验收正常；同 Block 重复候选会被 consumed 拒绝，远端 cwd 保持 `/data/home/nickhaoxu`。现有 `enable_ssh_warpification` 是总开关，设置页和 Command Palette 已有相应模式。

### Scope 与边界

- In scope：新增设备本地布尔设置及 Warpify 设置页开关、Command Palette enable/disable 入口；默认关闭。开启后，只有现有 SSH offer 的全部资格校验通过时才自动调用已验证的 bootstrap。
- 手动兼容：设置关闭时保留当前每次提示并点击的行为；SSH Warpify 总开关关闭、主机 denylist、认证中、旧 Block/session、重复触发、Agent/viewer、alt-screen、命令退出时均不得自动注入。
- 风险边界：这是用户明确 opt-in 的全局行为，仍依赖 prompt 启发式，不认证最终主机；因此开启后 `tx-jump` 等所有符合资格的 SSH 都可能尝试接入。失败保持普通 SSH，不修改 SSH 配置、远端 RC 或公司策略。
- Out of scope：per-host 自动白名单、自动为 tx-jump 绕过工具限制、修改 prompt 识别规则、恢复远端 server、提交或推送 Git。

### 成功标准

- [ ] 设置关闭时，219 继续展示手动确认且不自动写 PTY；设置开启时，同一候选自动 Warpify 且不显示可点击的陈旧入口。
- [ ] 自动路径复用现有 Block/session/policy guard 和 bootstrap；并发候选、重复通知或设置切换不产生二次注入。
- [ ] Warpify 设置页和 Command Palette 均可发现并切换该设置，设置持久化为设备本地、默认关闭。
- [ ] 新增/相关回归、格式、代码审查与 GUI 构建通过；重启新版后实际 219 无需点击即可接入。
- [ ] 最终交付明确全局启用的启发式风险，且不把它描述为目标主机身份验证。

### Artifact 决策

- `spec.md`：required；这是 SSH 接入行为与安全边界变化，修订现有 `docs/design-docs/terminal/restricted-ssh-warpify/spec.md`，不创建第二事实来源。
- `tasks.md`：required；涉及 settings model、设置 UI/Command Palette、终端入口和测试，批准 spec 后用 task-planning 修订现有任务清单。

### 质量门禁

- [ ] Gate: Plan Approval
- [ ] Gate: Spec Review
- [ ] Gate: Code Review
- [ ] Gate: Final Review

### 步骤

1. [x] 调研现有总开关、设置 UI、Command Palette 与已验证 offer/guard 路径。
2. [ ] 获得本范围的 Plan Approval（暂缓）。
3. [ ] 用 spec-writing 修订受影响的目标、需求、接口、机制、风险与验证章节并完成 Spec Review。
4. [ ] 调用 task-planning 修订 tasks.md。
5. [ ] 完成 tasks.md 中的所有任务。
6. [ ] 定向测试、格式、GUI 构建和 Code Review。
7. [ ] 重启并实际验证 219 自动接入，完成 Final Review；未经明确要求不提交推送。

以下“一键手动接入”范围已于本次设置任务前完成，仅保留为历史与实现基础。

## 2026-09-11 一键接入范围修订（已完成）

### 目标与当前状态

- 目标：为现有 RemoteCommand 嵌套 SSH 补充安全的客户端接入入口，复用现有 subshell bootstrap，消除反复手贴控制序列的需要；在可验证就绪条件下支持显式启用的自动接入。
- 状态：completed（安全的一键接入范围）；用户实际 219 验证正常。无人确认自动接入仍未实施，每次 SSH 由用户确认。
- 已确认：用户手动接入后 pwd、Tab 正常；目录标签 ~ 已按远端 HOME 展开，本次日志没有 Remote directory listing failed。原版本地 checkout c12e213 的 wrapper/warpify/bootstrap 与 Agenterm 对应路径一致；RemoteCommand 回退是原有行为。
- 原版 ReadyToWarpify 允许 LastLogin/NonSshOutput 触发，不能单独证明嵌套 SSH 已到最终目标，不作为自动注入的充分条件。

### Scope 与边界

- In scope：现有 SSH 会话的一键接入入口、显式 opt-in 自动接入的安全判定、会话/Block 绑定、取消/超时/重复触发处理、定向测试与真实 219 验证。
- Out of scope：tx-jump bootstrap、修改 SSH 配置、远端 RC 文件或公司策略、绕过 MFA、恢复 tmux/远端 server、任意 RemoteCommand 的通用自动注入、GitHub 提交。
- 不变量：复用 trigger_subshell_bootstrap，不合成 Bootstrapped；不在认证/中间跳板阶段注入；失败保留普通 SSH，退出会话后取消延迟动作；不重复接入已 Warpified 的会话。
- 自动接入候选条件必须在设计中明确证据与局限；不能确认最终 shell 就绪时只提供用户主动的一键操作。若没有不改变既有连接方式的安全自动方案，明确报告该限制，不宣称自动目标完成。
- 需要重新批准：SSH/远端配置修改、扩大到其他主机或安装远端组件。

### 成功标准

- [x] 无需手贴 DCS 即可通过客户端入口接入 219；用户确认 Warpify 流程正常，日志中的远端 cwd 正确。
- [ ] 自动模式仅显式启用且就绪条件成立时触发；认证、失败、旧 Block、退出、已接入场景均不注入。
- [x] 新旧回归测试、格式检查、GUI 构建通过；保留运行日志证据，不将一键成功当作自动成功。
- [x] 最终报告交付行为、测试、运行验证和剩余自动接入限制；更新 artifacts。

### Artifact 决策

- spec.md：required；这是跨检测/输入/接入状态的行为变化。用 spec-writing 修订现有 docs/design-docs/terminal/restricted-ssh-warpify/spec.md，将旧 tx-jump 草稿明确标为已替代，保留一个事实来源。
- tasks.md：required；设计确定后用 task-planning 拆分状态与入口、测试等实现任务。

### 质量门禁

- [x] Gate: Plan Approval — 用户 go 批准。
- [x] Gate: Spec Review — 独立审查 PASS，用户 go 批准。
- [x] Gate: Code Review — Standard 审查及 3 项 P2 修复复查通过；最终相关测试 149/149 通过。
- [x] Gate: Final Review — 用户实际验证一键接入正常；自动目标明确保留未完成。

### 步骤

1. [x] 对照原版源码及本次日志，区分目录标签错误与 RemoteCommand 接入缺口。
2. [x] spec-writing 与独立 spec-review 完成，用户批准。无人确认自动注入仍不实施，先交付安全回退中的主动入口。
3. [x] 调用 task-planning 生成 tasks.md。
4. [x] 完成 tasks.md 中的所有任务。
5. [x] 定向测试、格式、build、code-review 与实际 219 验证完成。
6. [x] Final Review；未经用户明确要求未提交推送。

以下保留先前诊断记录；当前待执行契约以上方修订为准。

## 2026-09-11 继续

- 用户确认手动接入后 pwd 与 ls+Tab 正常；日志显示远端 Precmd/Block/Input 均为远端目录，错误请求另行携带本机路径。
- 当前获批修复：DirectoryFetcher 对显示路径 ~ 使用本机 HOME；改为会话 HOME，补充路径展开测试和目录标签来源日志。git blame 证明旧逻辑已存在于初始提交 6d411b0，并非本轮修改引入；自动接入流程尚未验证，不混为一谈。
- [x] 目录标签修复验证：DirectoryFetcher 5 个测试、builtins 3 个测试通过；定向 rustfmt 与 diff check 通过；GUI 构建打包签名完成并重启，目录标签模块 INFO 已启用，待实际远端复测。
- 用户要求继续昨天的工作；Git 基线为 21261c5。
- 下一步：先用回归测试验证跨会话 Precmd 缓存污染，确认后修复会话隔离，再验证 219 基础命令与补全。自动接入仍未实现，不与当前缓存修复混合。
- [x] Red：旧代码测试实际返回 session 101，预期 202；跨会话缓存污染复现。
- [x] Green：只复用同 session 的 prompt 缓存；13 个 in-band 测试通过，包含远端完整 prompt 到达后的目录恢复与同 session 缓存复用。
- [x] 定向回归与首轮构建：BlockList 48、TerminalModel 51 通过；构建签名成功。
- [ ] 实际 219 验证：首轮仍出现远端查询本地目录，缓存隔离不是已确认的完整根因。用户授权增加 session/pwd/in-band 日志，待日志版构建后关联 Precmd、缓存、Block/Input、OSC 7 与补全请求，区分旧请求和当前会话。不得绕过跳板机策略，不用 Computer Use，不改远端配置。
- [x] 日志版编译验证：重新编译后的 in-band 13、BlockList 48、TerminalModel 51 测试通过（有重叠）；定向 rustfmt、diff check 通过；GUI bundle/sign 成功。00:30 重启为 PID 52772，仅诊断模块开启 INFO，待用户实际连接后读取链路。

## 2026-09-10 已批准的范围更新

用户已批准修复顶部 Paste 动作和增加后台命令诊断日志。旧 tx-jump 工具兼容方案暂停：用户确认工具受公司策略限制，不绕过限制。

219 直接交互 ssh 可用；真实 Bash bootstrap 已验证 Bootstrapped、Preexec、CommandFinished、Precmd 和退出码 0/1；GUI 手动接入后补全仍失败。目录查询和 generator 编码隔离验证成功，失败根因待新日志定位。

- 当前范围：Paste 使用现有 CustomAction；增加目录路径、退出码、输出长度、请求 ID 与取消事件诊断，不记录完整命令、输出或凭据。不改调度语义或远端配置。
- spec.md/tasks.md：本次为小范围菜单与日志修改，无需新增。
- [x] Plan Approval：用户明确回复“可以”。
- [x] 编译及定向验证：cargo check 成功；菜单测试 1/1，in-band executor 测试 9/9 通过；rustfmt 和 git diff --check 通过。
- [x] 本地 diff 自查，./script/run --dont-open 构建打包签名成功。
- [ ] 真实菜单/219 验证：新版已准备，后台失败根因仍待诊断日志。
- [ ] Final Review：区分菜单修复与后台问题仍待定位。

以下是历史 tx-jump 计划，剩余步骤不再作为当前执行指令。

## 目标

让 `ssh tx-jump` 建立可识别的 Warpify 会话，同时保留现有 SSH 配置、MFA 和 ControlMaster 行为。

## 当前状态

- 状态：in_progress
- 当前步骤：复核原始证据，定位远端工具依赖不可用。
- 上次同步：2026-09-10，固定字节对照证明 DCS 10 字节与 OSC 522 字节完整返回。
- 下一步：评估缺少 od、tr、sed、stty 时的真实 Shell 生命周期支持；禁止提前合成 Bootstrapped。

## Scope 与边界

- In scope：SSH wrapper 对 `tx-jump` 这类普通跳板机的 Warpify 支持、针对该行为的回归覆盖、定向构建和真实本机 smoke test。
- Out of scope：修改公司跳板机策略、绕过 Pin+Token/MFA、安装远端常驻组件、上传或改变线上机器配置。
- 假设：`tx-jump` 没有配置 `RemoteCommand`，当前问题收敛于 Warpify bootstrap 与跳板机交互 shell 的协议兼容性。
- 关键约束/不变量：普通 SSH 和未支持的 `RemoteCommand` 必须保持安全地回退；不得记录或暴露认证信息；ControlMaster 仍可复用且不会被 Warp 错误关闭。
- 需要重新获批的变化：若实现需要修改 `~/.ssh/config`，或无法在不改变 `219` 连接语义的前提下支持现有任意 RemoteCommand，则先给出精确配置变更并等待确认。

## 成功标准

- [ ] `tx-jump` 不再因 Warpify bootstrap 的兼容性问题静默失败；客户端可明确判定并呈现 Warpify 状态或安全回退原因。
- [ ] Pin+Token、公司跳板规则和既有 ControlMaster 不被绕过或破坏。
- [ ] 为新分支增加可重复的定向验证，并通过相关 Rust/脚本检查与 GUI build smoke test。
- [ ] 最终交付包含变更摘要、验证证据、确切用户配置动作（若需要）及剩余兼容性风险。
- [ ] 最终 artifacts 反映真实状态：完成、跳过、阻塞和验证结果。

## Artifact 决策

- `spec.md`: not required — 是既有 SSH wrapper 的兼容性修复，不引入独立的长期产品设计。
- `tasks.md`: not required — 本阶段聚焦一条既有 bootstrap 路径及其回归验证，预期为小范围修复。

## 质量门禁

- [x] Gate: Plan Approval — 用户于 2026-09-10 批准。
- [ ] Gate: Code Review — 涉及 SSH transport 和认证边界的代码改动。
- [ ] Gate: Final Review — 批准最终结果、验证证据和提交准备状态。

## 步骤

1. [x] 调研 SSH wrapper、ControlMaster 重用、`RemoteCommand` 回退及本机 `tx-jump`/`219` 配置。
2. [x] 复核传输：DCS/OSC 固定字节完整返回。原 DCS 探针错误展开了 $d7b7d。实际 wrapper 前缀生成空载荷，直接探针发现 od/tr 不可用、command -p 触发 PATH 提示；sed/stty 也未解析到可用命令。
3. [~] 研究工具依赖兼容修复并添加回归覆盖。已撤销分片协议假设；合成握手实验不是完整 Shell 集成的证明。
4. [ ] 运行定向回归、格式化/Clippy 和 GUI build smoke test。
5. [ ] 代码评审 → 调用 `code-review` → Gate: Code Review。
6. [ ] 准备最终交接 → Gate: Final Review。
