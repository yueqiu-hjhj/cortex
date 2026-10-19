# Cortex 自用启用计划（2026-10-19）

> 目标：**这轮做完就开始日常使用**。原则：一切为自用服务，不做任何发布/推广。
> 基线：`0885ad5`（Phase 1 收尾 —— 1.4 MCP npm 包装、1.1 Anki/Obsidian 无头迁移；改进计划 13/13；P2 三件全部落地）。

## 0. 你现在手里已有什么（不用再做）

| 能力 | 状态 |
|---|---|
| 入库管线（PDF/Office/网页/YouTube/录音 → 学科/主题/来源 → 分块+向量） | ✅ 可用 |
| 学习材料 9 件套（cheatsheet / flashcards+FSRS / quiz / audio overview / slides / infographic / mindmap / video / 双语精读） | ✅ 代码级 |
| 科研件（PRISMA 8 维提取、跨论文张力分析、跨书技能卡、token 记账面板） | ✅ 代码级，**未接真模型冒烟** |
| Anki `.apkg` 导入导出 + 无头迁移脚本；Obsidian 导出 + 无头导入脚本 | ✅ |
| 笔记 `[[双链]]`+backlinks、MCP 13 工具、`.cortex-pkg` 归档、age+rclone 加密备份 | ✅ 可用 |
| 录音侧（iOS 录音、WhisperX homelab 转写、live sync） | ✅ 代码级（homelab 服务需自起） |

「代码级」= 测试全绿但没接真模型端到端跑过——这正是第一阶段要补的。

## 1. 第一阶段：启用检查单（做完即开始用，约 1 个工作日）

### A1. 模型接入 + 全任务冒烟 ⭐ 最关键

设置里配好 key（Gemini / OpenRouter / OpenAI / Claude 任一，或本地 Ollama），
再给**每类任务**各配一个模型（便宜模型跑 flashcard/quiz，强模型跑 chat/PRISMA）。
然后按顺序过一遍冒烟清单——每项不过就先修再用：

| # | 冒烟项 | 操作 | 验收标准 |
|---|---|---|---|
| 1 | 入库 | 拖一个真实 PDF 进学科 | 状态 ready，能全文检索 |
| 2 | 引用 chat | 问一个 PDF 里的内容 | 回答带引用角标，点击跳原文 |
| 3 | Cheatsheet | 对该来源生成 | 结构化产出（不触发离线 fallback） |
| 4 | Flashcards | 生成 | 卡片进入 FSRS 复习队列 |
| 5 | Quiz | 生成并作答 | 可评分、错题回炉 |
| 6 | Audio overview | 生成双人播客 | 音频可播（需支持 TTS 的模型） |
| 7 | PRISMA | 选 2–3 篇论文跑 8 维提取 | 每个值都有 verbatim evidence + excerpt 回链 |
| 8 | 张力分析 | 对同批论文建 run | 张力条目带原文引用，可导出 Markdown |
| 9 | 技能卡 | AnalyticsView 点「沉淀」 | 跨书概念蒸馏成 when_to_use/steps 卡 |
| 10 | Token 面板 | 完成上述操作后查看 | AnalyticsView 有按天/按类型的消耗条形 |
| 11 | MCP | Claude Code 指到 `cortex-mcp` | 13 个只读工具可调你的库 |

### A2. 数据迁移实测（把旧数据搬进来）

前提：**关闭 Cortex**（SQLite 单写者）。先 dry-run 看计划，再实跑：

```bash
node scripts/cortex-import-anki.mjs <你的牌组>.apkg --dry-run
node scripts/cortex-import-anki.mjs <你的牌组>.apkg        # 每 deck 一个学科
node scripts/cortex-import-obsidian.mjs <vault 路径> --by-folder --dry-run
node scripts/cortex-import-obsidian.mjs <vault 路径> --by-folder
```

迁移后打开 App 抽查：卡片在复习队列里、笔记的 backlinks 面板正常。

### A3. 备份（学习数据无价，必须做）

设置里填 age 收件人公钥 + rclone 远端，启用加密备份。**备份 ≠ 可恢复**：
做完第一次备份后，手动做一次「删库 → 恢复」演练，确认真能还原再开始日常使用。

### A4. 可选增强（按需，不阻塞启用）

- **SearXNG**：自建一个（homelab/ 有现成配置）→ 解锁 chat 联网模式 + 图片抓取
- **WhisperX homelab**：有上课录音需求再起；配 iOS 录音 + live sync
- **Ollama**：本地模型做断网兜底（更强做法是第二阶段的降级链）

## 2. 第二阶段：用起来之后按痛点补（自用向精选，按建议顺序）

来自路线图 Phase 2，只保留自用有感的：

| 序 | 项 | 为什么自用值得 | 量级 |
|---|---|---|---|
| 1 | 多 provider 降级链（2.9） | key 配额用尽/断网时自动切备用模型，自用稳定性 | 1 周 |
| 2 | 考试倒计时 Banner（2.11） | ≤7 天橙色/≤3 天红色，学习闭环最直观 | 2 天 |
| 3 | 自定义主题（2.10） | 自己看着舒服，天天用 | 3 天 |
| 4 | 图表自动解释 + 公式修复（2.6） | 读文献时图表/公式不再是死图 | 2 周 |
| 5 | 写作升格批改（2.1） | 写论文时逐句升格（工作量大，放到实际写论文前做） | 2 周 |
| 6 | 复杂 PDF Docling 解析（2.12） | 若冒烟发现 PDF 解析烂再做；先观察 | 2 周 |
| 7 | 跨论文自定义 schema 表 + CSV（2.4） | 做系统综述汇总数据时 | 2 周 |

> 注：2.8「六类模型分别配置」——应用已有 per-task models 设置，先用起来核对缺口，够用就不做。

## 3. 并行会话在飞（不排进计划，收尾后验证）

Zotero 实时链接（`0052`）+ arXiv/PubMed 一等公民 connector（= 路线图 2.2/2.3）
正由另一个会话开发中。**收尾后**补一个真 API 冒烟：arXiv 检索 → 一键入库 →
进正常阅读管线 → PRISMA 提取。

## 4. 明确不做

- **发布类**：skills.sh、Homebrew 启用、Reddit/HN、Demo 视频（`0885ad5` 已定）
- **Phase 3 全部**：web/CLI 多端、Channels、benchmark、Skills 库——分发与协作需求
- **Phase 4 部分**：iOS 上架、OpenTutor/校园订阅、企业版、游戏化
- **反向清单全部**：Python/R kernel、SSH/Slurm、BioNeMo、浏览器扩展、自建 web 渲染……

## 5. 开始前需要你确认的三件事

1. **模型**：用哪家 key（Gemini / OpenRouter / OpenAI / Claude）？要不要本地 Ollama 兜底？
2. **迁移**：有真实的 Anki 牌组 / Obsidian 库要搬吗？给我路径，我带你先 dry-run 跑一遍。
3. **备份**：rclone 远端用哪个（网盘 / WebDAV / 移动硬盘）？我帮你生成 age 密钥对。
