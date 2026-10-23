# Cortex vs SourceWeft vs Gloss — 深度比较（2026-10-24）

> 事实来源：SourceWeft 官网 + GitHub README/API（2026-10-23 抓取，仓库当日仍在推送）；Cortex 取自本仓库代码与测试；**Gloss 见 §1.2 的信息缺口说明**——公开渠道无法唯一确认，本文只比较仓库里可验证的三条借鉴模式。

## 0. 结论

- **SourceWeft 是目前最贴近 Cortex 学生场景的竞品**：它明确把"课程材料/视频/PDF → 学习指南、FAQ、测验草稿"写进用例，还带多 agent 委派、技能库、沙箱执行。Khoj 只是"入口广"，SourceWeft 是"能产出"。
- **Cortex 仍然赢在闭环**：SourceWeft 产出学习指南就结束了；Cortex 把它送进 FSRS 复习队列，用 BKT 掌握度决定下一步练什么。
- **Gloss 的三条模式，Cortex 已经全部吸收**（三态 scope、删除前恢复目录、能力边界表）。它是"安全与诚实"的参照，不是功能竞品。

---

## 1. 事实基线

### 1.1 SourceWeft（可验证）

| | 事实 |
|---|---|
| 定位 | Open-source, self-hostable alternative to **ChatGPT and NotebookLM** |
| 仓库 | `SourceWeft/SourceWeft`，308 星 / 26 fork / 45 open issues，2026-05 创建，**2026-10-23 仍在推送** |
| 语言 / 许可 | TypeScript / **Apache-2.0**（`enterprise/` 目录除外，另有商业许可） |
| 客户端 | Web + macOS(ARM) / Windows(x64) / Linux(x64) 桌面 |
| 部署 | Docker 自托管包（web + API + 后台任务 + 数据库 + 文件存储）；官方云可选 |
| 模型 | 多 provider + **BYOK**；chat 与 indexing 可分别配模型 |
| 知识源 | PDF、网页、笔记、**YouTube**、**Notion、Google Drive、Gmail、Slack** |
| Agent | **多 agent + 子 agent 委派**（research/exploration/planning/execution），共享工作上下文 |
| 技能 | 内置（演示文稿、HTML 页、图片、会议纪要、学习）+ 自定义技能；官网挂 14,837 个技能 |
| 工具 | MCP（官网挂 32,539 个 server）、web search、web extraction、**沙箱执行**（需配置 runtime + 审批） |
| 产出 | 报告、演示文稿、HTML 页、学习指南、音频概览、图片 |
| 协作 | 共享 workspace + 角色/权限（团队向） |

### 1.2 Gloss（信息缺口，必须说清）

公开渠道有多个同名产品（Gloss Notes/NoxDrop 尚未发布、Gloss Ink、GlossAI 等），**没有一个能与 Cortex 仓库里的三条引用对上**。因此本文不臆测它的功能，只比较这三条可验证的借鉴模式：

1. **检索 scope 的第三态**（笔记 / 网络 / 都不用，纯模型回答）——Cortex 代码注释直接写「Gloss's third state」；
2. **删除前的恢复目录**（先落盘再物理删）——Cortex 注释写「Gloss-grade safety」；
3. **README 能力边界表**（✅/🚧/❓ + boundary 列）——Cortex 改进计划写「仿 Gloss」。

> 如果你给我 Gloss 的官网或仓库，我可以补一份和 SourceWeft 同等深度的对比；现在这一节只对"可验证的三条"负责。

---

## 2. Cortex vs SourceWeft（十个维度）

### 2.1 定位

| | SourceWeft | Cortex |
|---|---|---|
| 一句话 | 你的 AI 工作站：研究、规划、创作、执行 | 本地学习 OS：学、练、考、写 |
| 典型任务 | 研究简报、学习指南、演示文稿 | 复习排期、掌握度诊断、系统综述 |
| 终点 | 交付一份成果 | 让知识留在脑子里 |

**判定**：SourceWeft 的终点是"文件"，Cortex 的终点是"能力"。两者在"生成学习指南"这一步重叠，之后分道扬镳。

### 2.2 架构与部署

SourceWeft：Docker 包（web + API + jobs + DB + 存储），也可用官方云；桌面端是客户端壳。
Cortex：Tauri 单二进制 + 单 SQLite 文件，无服务端。

**判定**：SourceWeft 换来"多端 + 团队"，代价是运维一套服务；Cortex 换来"零运维 + 离线"，代价是没有远程入口。

### 2.3 Agent 与多 agent 委派（SourceWeft 领先）

SourceWeft：**主 agent 可以把 research / exploration / planning / execution 分给子 agent，再汇总**，共享工作上下文——这正是你上次问的"一个对话里自动切换多个 agent"。
Cortex：同一线程**手动**切换 agent（上下文保留），但没有 `delegate` 工具、没有编排器；权限是 per-agent 的，自动委派需要先定"能不能提权"。

**判定**：SourceWeft 领先一档，而且它的存在证明了这个需求是真的。

### 2.4 工具与执行（SourceWeft 有沙箱，Cortex 有分级）

| | SourceWeft | Cortex |
|---|---|---|
| 执行 | **沙箱**：批准的命令在隔离临时环境跑，产物收回工作区 | 无 shell；文件工具只读写工作目录 |
| 权限 | 审批制（命令级） | 四级（read < write < network < files）+ 工具白名单 |
| 写操作 | 直接产出文件 | 笔记走**提案**（diff → 采纳才落库） |
| 文件 | 沙箱内 | `read_file/write_file/edit_file/list_dir/search_files`，路径强制在 workspace 内 |

**判定**：SourceWeft 的"能跑命令"更接近 Claude Code；Cortex 的"分级 + 提案"更保守。**沙箱 + 审批**是 Cortex 值得补的一课。

### 2.5 知识源与连接器（SourceWeft 更宽）

SourceWeft：PDF、网页、笔记、**YouTube**、Notion、**Google Drive**、**Gmail**、**Slack**。
Cortex：PDF/DOCX/PPTX/EPUB/图片 OCR；Notion、GitHub Issues、Zotero；arXiv/PubMed 检索。

**判定**：SourceWeft 赢在"日常工具"（Drive/Gmail/Slack/YouTube）；Cortex 赢在"学术工具"（Zotero/arXiv/PubMed）。**YouTube 字幕**对学习场景价值很高，Cortex 缺。

### 2.6 引用与可核验性

SourceWeft：引用可回溯到原始材料（"follow citations back to the original material"）。
Cortex：引用带**冻结快照**、可重锚、编造引用会被丢弃并记录、答案健康度分开统计。

**判定**：SourceWeft 的引用是"能点回去"；Cortex 的是"能核验、能修复"。写论文场景 Cortex 更强；日常研究简报两者够用。

### 2.7 学习闭环（Cortex 独占）

SourceWeft：学习指南、FAQ、测验草稿——**产出即结束**，没有排期、没有掌握度、没有复习。
Cortex：FSRS 排期 + BKT 掌握度 + 材料（速查表/测验/音频/幻灯片/信息图）+ 考试倒计时 + 英语训练 + 写作工具箱，材料直接进复习队列。

**判定**：9:0。这是 Cortex 的护城河，也是 SourceWeft 用例表里"study guides"止步的地方。

### 2.8 科研工作流（Cortex 独占）

SourceWeft：研究简报 + 引用，没有系统综述流程。
Cortex：PRISMA 8 维（verbatim 证据 + 摘录回链 + 导出）、撤稿检查、期刊信号（DOAJ/core）、`.cortex-pkg` 归档、vault 加密。

**判定**：写综述/投稿，Cortex 领先一档。

### 2.9 协作与多端

SourceWeft：共享 workspace + 角色权限 + Web/三桌面端。
Cortex：单用户；桌面 + iOS 录音 + live sync。

**判定**：SourceWeft 完胜。Cortex 的"自用"定位在这里是主动放弃，不是缺陷。

### 2.10 成熟度与许可

SourceWeft：308 星但**日更**、有 Discord、有企业版计费；Apache-2.0（企业目录除外）。
Cortex：624 测试、严格 CI、单人维护。

**判定**：SourceWeft 生态更活跃；Cortex 的工程质量更高但覆盖面窄。**许可上 SourceWeft 比 Khoj 友好**：Apache-2.0 可以借鉴实现（保留声明），AGPL 不行。

---

## 3. Cortex vs Gloss（只比可验证的三条）

| 模式 | Gloss 的做法（据仓库引用） | Cortex 现状 | 判定 |
|---|---|---|---|
| 检索三态 | 笔记 / 网络 / **都不用**（第三态是显式状态，不是"没检索到"） | `level == "none"` → `none_scope`，跳过检索，元数据标记 + UI「未检索」徽章 | ✅ 已吸收 |
| 删除安全 | 删除前先落一份**恢复目录**，撤销日志不是唯一退路 | `delete_subject` 先把整行集 + 附件落到 `.deleted-<id>-<ts>` 再物理删 | ✅ 已吸收 |
| 能力边界表 | README 用 ✅/🚧/❓ + boundary 列诚实标注能力 | README「Capability boundary」表，含 gate 列（automated gate / run-specific） | ✅ 已吸收 |

**结论**：Gloss 对 Cortex 的价值不是功能，而是三条"产品诚实度"的标尺，而且已经全部落地。它不构成竞品威胁。

---

## 4. 三方对照（仅可验证维度）

| 维度 | Cortex | SourceWeft | Gloss |
|---|---|---|---|
| 定位 | 学习 OS | AI 工作站 | 安全/诚实参照 |
| 多 agent 自动委派 | ❌（手动切换） | ✅ | 未知 |
| 沙箱执行 | ❌ | ✅ | 未知 |
| 技能库 | 4 个内置 | 内置 + 自定义 + 14k 目录 | 未知 |
| 连接器 | 学术向（Zotero/Notion/GitHub） | 日常向（Drive/Gmail/Slack/YouTube） | 未知 |
| 引用可核验 | ✅ 快照/重锚/丢弃 | ✅ 可回溯 | 未知 |
| 学习闭环 | ✅ FSRS+BKT | ❌ | 未知 |
| 科研证据链 | ✅ PRISMA+撤稿 | ❌ | 未知 |
| 多端 | 桌面 + iOS 录音 | Web + 3 桌面 | 未知 |
| 部署 | 单二进制 | Docker/云 | 未知 |
| 许可 | 私有 | Apache-2.0 | 未知 |

---

## 5. 威胁评估

**SourceWeft 的威胁高于 Khoj。** 原因不是功能更多，而是它**直接瞄准学生用例**（course material → study guides），并且能产出可交付物。一个学生如果只需要"把课件变成学习指南"，SourceWeft 今天就够用，而且有 Web 端、有引用、有技能库。

**但它仍然不构成替代**：学习指南生成完就结束了，没有"什么时候复习、哪里没掌握、考试前练什么"。Cortex 的护城河不在"生成"，在"生成之后的排期与诊断"。

**真正的风险**：SourceWeft 的产出足够好时，用户会**只把它当生成器**，把 Cortex 当复习器——两个工具切换的成本，会慢慢把 Cortex 挤成"周末才打开"的应用。

---

## 6. 值得借鉴（按性价比）

1. **子 agent 委派**（`delegate(agent_id, task)`）。SourceWeft 已经验证需求；Cortex 只差一个工具 + 权限继承规则（子 ≤ 父）+ 运行记录里标注子 run。这是你上次问的"自动切换多 agent"的正解。
2. **沙箱执行 + 审批**。补上"不能跑命令"的短板：命令白名单 + 临时目录 + 产物回收 + 每次批准。比开放 shell 安全，比完全不做实用。
3. **YouTube 字幕作为知识源**。学生场景里视频课占比高；拿到 transcript 就能进现有分块/检索链路。
4. **技能的可分享格式**。Cortex 只有 4 个内置技能；把技能定义做成可导入/导出的文件（带版本与来源），比做市场更符合自用定位。

## 7. 不该借鉴

- **Docker + 独立数据库的部署形态**：Cortex 的单文件是核心优势。
- **团队/角色/计费**：自用不需要，且会拖慢一切。
- **Web + 三桌面端的客户端矩阵**：维护成本高；一个只读 Web 页足够覆盖手机场景。
- **"产出即结束"的产品闭环**：那正是 Cortex 要避免的。

---

## 8. 结论

- **SourceWeft**：最像"下一代 NotebookLM"，也是目前对学生场景威胁最大的产品。它的多 agent 委派与沙箱执行值得学，但它的终点是文件，不是能力。
- **Gloss**：不是竞品，是三条产品诚实度的标尺；Cortex 已经全部做到。
- **Cortex**：不该去比"谁生成的指南更好"，而该把"生成之后"做得无人能替——排期、诊断、证据链。**别人把生成做成商品，你把留存做成壁垒。**
