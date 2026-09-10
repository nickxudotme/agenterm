# 代码评审报告

## 评审范围

- 意图：已批准 spec 的 SSH 主动接入入口，非无人确认自动注入。
- 文件：terminal event/model/ssh/view/warpify 及新测试；先前目录日志修改作为上下文。
- Tier：standard（终端关键路径、跨子目录）。受并发槽位限制，5 个领域分批审查。
- 审查者：review_perf、review_robustness、review_standards、review_spec_impl、review_proof；对应独立 validator 已完成。

## 结论：PASS（3 项 P2 均已修复并经对应 validator 复查；编译与运行验收另行完成）

## P2 问题

1. 全历史扫描：perf(P1)/robustness(P2) 合并，经 validate_perf confirmed 并降为 P2，confidence 98。Completed 后每批序列化全SSH历史产生确定性能退化，但未测得严重卡顿。改为有界尾部判断时必须保留认证软换行安全边界。
2. 拒绝日志缺失：spec reviewer，validate_diagnostics confirmed P2 confidence 99。Ready 缺host/session/旧Block早返回没有原因，不满足spec6.5。
3. UI接线测试缺口：spec/proof reviewers，validate_test_gap confirmed P2 confidence 96。现有PTY spy覆盖展示与拒绝，但缺成功/重复三入口及非SSH回归。不是已证实功能故障。

Standards 无发现；Proof 无确定控制流或信任边界缺陷。后续修改与验证完成后再更新最终状态。

## 修复复查

- validate_perf：有界尾部读取、丢弃截断逻辑行并保守处理无硬换行认证；原 P2 解决，confidence 97。
- validate_diagnostics：missing-host / missing-session / stale-block 均有标识与固定原因，不含认证或输出；原 P2 解决。
- validate_test_gap：新增三入口首次及重复 PTY 事件断言、旧 Block 拒绝、非 SSH 回归；原 P2 解决，confidence 93。footer 子视图真实点击及按键绑定保留 GUI 验收边界。
- 首轮旧代码快照编译通过；140 tests passed，1 分批 prompt 模拟测试失败，正在诊断；最终代码尚需重编译，不能以静态 PASS 代替测试通过。
- 该失败定位到 max_cursor_point 在 grid finalize 才更新，SSH 检测原本在其前读取。现移至 finalize 后且保留失败断言；review_proof 针对性复查通过，未增加 PTY 写入或锁，最终测试正在执行。
- 最终版本 nextest run f46b4ae6-c8f2-4cd2-88f3-6319b9ac9ae2：149/149 通过，包括原失败测试和全部新增入口测试；格式/diff 检查通过。真实 GUI 接入仍待用户验收。

## Validator 驳回

无。日志finding的依据收窄到spec6.5，性能finding从P1降级到P2；未以缺测试推断已有功能错误。
