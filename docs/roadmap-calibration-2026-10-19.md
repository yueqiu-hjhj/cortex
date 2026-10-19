# 路线图校准（2026-10-19 对 `cortex-integrated-roadmap-2026-10-19.md` 的核对）

> 综合路线图基线是 `ab6710b`（token dashboard）。本文件校正三件事：
> ① 基线之后又落地了两个功能；② 两处状态与代码不符；③ 给 Phase 1 的
> 执行建议（哪些可 repo 侧完成、哪些是用户动作）。

## 1. 基线之后的新提交（路线图未计入）

| 提交 | 功能 | 路线图中的条目 |
|---|---|---|
| `65f4c59` | **文献张力分析**（0050：多篇论文对抗性比较，paper id 防幻觉校验，无 verbatim 引用拒收，Markdown 导出，Research 面板） | 计划外新增（不在 29 项目对照里，知源式能力） |
| `49f9a24` | **跨书技能卡沉淀**（0051：cross_book 跨书概念 → 模型蒸馏成 when_to_use/steps 技能卡） | §2.3 #14「🟡 短期补」**→ 已完成** |
| `ab6710b` | Token 消耗可视化（0049，估算记账 + AnalyticsView 卡片） | 已计入基线 |

## 2. 与代码不符的路线图状态

| 路线图说法 | 实际 |
|---|---|
| §2.2 表「Notion 式块编辑器 ✅」 | **❌ 不实**：MarkdownEditor 仍是普通 Markdown 编辑器（+ `[[双链]]`、反向链接）。原计划文档明确「完整 Notion 块编辑器：不做」（收益不确定）。发布材料里不要写这一条。 |
| §2.3 #13 写作升格批改 🟡 | 确认未做（VocabularyWorkbench ≠ 升格批改）。Phase 2.1 有效。 |
| §2.2 表「跨书知识网络 → 技能卡 🟡」 | **→ ✅**（`49f9a24`，见上） |

## 3. 第一批改进计划（13 项）状态核对

四个 P0 + 批次一二三全部关闭，细节见 `docs/improvement-plan-2026-10-18.md`：
MCP 服务端 ✅(`974970a`) · `.cortex-pkg` 可移植归档 ✅ · Studio 确定性 fallback ✅ ·
多 Agent 共享上下文 ✅(`ea30a1b`) · CI strict gate ✅(`062b647`) · PRISMA ✅(`74e16b3`)。

## 4. Phase 1 的可执行性拆分（repo 侧 vs 用户动作）

| # | 任务 | 谁来 |
|---|---|---|
| 1.4 | `cortex-mcp` npm 包装（bin + 下载解析 + README 配置片段） | **repo 侧，本轮已开工**（`packages/cortex-mcp/`） |
| 1.1 | Anki/Obsidian 迁移脚本 `scripts/cortex-import-*.mjs` | repo 侧可做（注意：应用内已有 anki.rs/obsidian.rs 的导入导出，脚本是无头迁移通道，对分发故事有价值） |
| 1.2 | skills.sh 发布 Anthropic Skills 包 | **用户动作**（发布 + 命名；按用户铁律用其本人 namespace，不用 `PndaMan/` scope） |
| 1.3 | Homebrew formula | **基础设施已存在**：`.github/workflows/release.yml` 的 publish-packages job 每个 tag 自动推 Homebrew cask 到 `PndaMan/homebrew-cortex` + AUR `cortex-bin`。注意：这是**上游**基础设置（发布者身份 Aidan McConnon）——若要在你的 fork 启用需换 tap 仓库与密钥 |
| 1.5/1.6 | Reddit/HN 帖子、Demo 视频 | **用户动作** |

**命名提醒**：路线图 1.2 的示例 `npx skills add PndaMan/cortex` 用了上游组织名——按「代码只推/归属用户自己仓库」的铁律，Skills 包与 npm scope 应使用用户本人 namespace（如 `@yueqiu-hjhj`），发布前确认。

## 5. 结论

综合路线图的战略判断（不追 Python/R kernel / SSH/Slurm；放大「桌面本地 × 学习 OS × 多模态 × 录音」交叉点）与两批调研结论一致，维持有效。执行顺序建议：先 1.4（本轮）→ 1.1 → 1.3（formula 骨架）→ 用户执行 1.2/1.5/1.6。
