# Cortex vs 九个综合研究/学习工作台：深度对比

**日期**：2026-10-22
**对手资料**：2026-10-22 抓取各项目官方 README（star 数为 2026-10-19 数据）
**Cortex 基准**：本地 `main` = `c202765`，含 **64 个未推送提交**（不是路线图快照，功能已远超 10-19 版）

> 对比口径：不只看"有没有"，还看**做到什么深度**、**为谁设计**、**代价是什么**。
> ✅/❌ 只能说明存在性，说明不了质量。

---

## 1. 一句话定位对照

| 项目 | 一句话 | 为谁 | 形态 |
|---|---|---|---|
| **Khoj** 37.7k | 个人第二大脑：索引你的一切，多端随问 | 知识工作者 | Web / 桌面 / Obsidian / Emacs / 手机 / WhatsApp |
| **STORM** 31.7k | 用"多视角提问 + 模拟对话"从零写维基式长文 | 研究者、写作者 | Python 库 + Streamlit |
| **GPT Researcher** 30.2k | 深度研究代理：规划→执行→发布带引用长报告 | 分析师、记者 | Web / API / Docker / pip / MCP |
| **SurfSense** 16.5k | 气隙本地 NotebookLM 替代，还能改你的文件 | 处理敏感文档的个人 | 桌面（Win/mac/Linux） |
| **PaperQA2** 9.4k | 科学文献的高精度 agentic RAG | 科研人员 | CLI + Python 库 |
| **Local Deep Researcher** 9.4k | 最小可复现的本地深度研究参考实现 | 学习者、二次开发者 | LangGraph Studio |
| **Local Deep Research** 9.3k | 自托管研究助手，隐私工程最硬 | 隐私敏感研究者 | Web / API / MCP / Docker |
| **AIPOCH Open-Science** 5.2k | 本地优先科研工作台：执行+溯源+打包 | 实验型科研团队 | 桌面 + CLI + 浏览器工作区 |
| **OpenScience** 4.1k | 科研工作台：Python/R/集群 + 领域专家 | 计算型科研团队 | 桌面 + CLI + 浏览器 |
| **Cortex** | 桌面本地学习 OS × 多模态生成 × 录音 × 科研证据链 | 学生 / 自学者 / 单干研究者 | 桌面（Tauri）+ iOS 录音 + live sync |

---

## 2. 逐个画像（强项 / 硬伤 / 对 Cortex 的威胁）

### 2.1 Khoj（37.7k）—— 多端最广
**架构**：Python/Django + Postgres/pgvector；自托管或云；桌面/浏览器/Obsidian/Emacs/手机/WhatsApp 六端。
**真强项**：① 接入面最广，索引 org-mode/Notion/Word/PDF/图片；② 自定义 agent（知识库+人格+模型+工具）；③ Automations（定时简报、通知到邮箱）；④ 本地或在线 LLM 随意切。
**硬伤**：**没有学习闭环**（无 SRS、无复习、无掌握度）；**没有学习材料生成器**（无 cheatsheet/quiz/导图/幻灯片）；科研侧只有"问答+引用"，无 PRISMA/证据链/跨论文工作流。
**对 Cortex 的威胁**：如果需求是"随时随地问我自己的资料"，Khoj 赢；如果需求是"学完要记住、要产出材料"，Khoj 直接出局。

### 2.2 STORM（31.7k）—— 提问方法最学术
**架构**：dspy 四模块（知识整理→大纲→成文→润色）；视角引导提问 + 模拟专家对话；Co-STORM 加动态思维导图 + 人机协同。
**真强项**：**"自动提出好问题"这件事做得最深**，有 NAACL/EMNLP 论文与 FreshWiki 数据集背书；输出维基式带引用长文。
**硬伤**：一次性生成，**没有持久库、没有阅读管理、没有复习**；无桌面应用；引用是机制性接地，无逐条核验；官方自己说"不能产出可发表成品"。
**对 Cortex 的威胁**：低。Cortex 的 deep research 已覆盖"课题→五源检索→筛选→综述初稿→引用校验"，STORM 的方法论可借鉴（多视角提问），产品形态不构成替代。

### 2.3 GPT Researcher（30.2k）—— 深度研究流水线最成熟
**架构**：Planner / Execution / Publisher 三段式，并行爬虫 agent；20+ 网页来源；本地文档（PDF/Office/CSV）；MCP client + 独立 `gptr-mcp` server；LangSmith 追踪。
**真强项**：**工程化最深的研究代理**：报告 2000+ 字、内联配图、PDF/Word 导出、`npx skills add` 一行接入 Claude Code；成本可控（约 $0.4/次）。
**硬伤**：**无持久知识库**（每次研究从零）；无学习闭环；官方明确"不推荐用于学术论文"；无桌面端；无多媒体学习材料（无播客/导图/卡片）。
**对 Cortex 的威胁**：中。一次性调研任务它更快；但调研完的**沉淀与复用**它没有——Cortex 的库 + PRISMA + 复习正是补这一段。

### 2.4 SurfSense（16.5k）—— 最直接的同形态对手 ⚠️
**架构**：桌面应用（Win/mac/Linux），本地索引、本地或远端模型、无账号；Apache-2.0；原云版已于 2026-10-18 下线，全面转桌面。
**真强项**：① **文件编辑**：Word 修订/批注、Excel 公式与图表、PPT 改字删页、PDF 表单填充/水印/合并拆分；② 格式覆盖广：扫描 PDF（OCR 支持中英日）、Office、CSV、图片、录音转写、播客（本机配音）、思维导图、闪卡、测验；③ 插件体系（Google/YouTube/TikTok/地图/亚马逊…）+ 第三方 MCP。
**硬伤**：① **闪卡只是"能生成"，没有间隔重复/掌握度**；② 无科研深度（无 PRISMA、无引用字符级定位、无跨论文表、无张力分析）；③ 自有插件需许可证（30 天试用）；④ 无 Anki 互通；⑤ 无复习/考试/画像等学习闭环。
**对 Cortex 的威胁**：**最高**。同样的"本地桌面 + NotebookLM 式"叙事，且文件编辑能力更强。差异化必须落在"学完记得住"（FSRS/完形/陪练）与"研究做得深"（PRISMA/证据链）——这两块它没有。

### 2.5 PaperQA2（9.4k）—— 证据检索精度最强
**架构**：agentic RAG（ToolSelector 可任意顺序调工具）；RCS = LLM 重排 + 上下文摘要；默认 gpt-4o + text-embedding-3-small，可换本地（Ollama/llama.cpp/st-）。
**真强项**：① **LitQA2 上宣称超越人类**（问答/摘要/矛盾检测）；② 引用带页码，证据相关性阈值可调；③ **元数据自动校验**（Semantic Scholar/Crossref/Unpaywall，含引用数、**撤稿检查**）；④ 多模态（表格/图/公式）。
**硬伤**：无 GUI（CLI+库）；**无文献管理**（要自备 PDF）；无学习材料；无复习；7B 小模型效果差；无多端。
**对 Cortex 的威胁**：低（形态不同），但它是**证据层的标尺**——Cortex 的 PRISMA/张力若想被认真对待，检索精度应以此为参照。

### 2.6 Local Deep Researcher（9.4k）—— 教学参考，不构成竞争
**架构**：LangGraph 循环（生成查询→搜索→摘要→反思缺口→再查），Ollama/LMStudio，DuckDuckGo/SearXNG/Tavily/Perplexity。
**真强项**：**最小、最可读**的深度研究实现；完全本地；一次跑通只需一个 `.env`。
**硬伤**：无库、无引用核验、无 UI（借用 LangGraph Studio）、循环 3 次即止。
**对 Cortex 的威胁**：无。价值是"想自己改研究循环时的教学样例"。

### 2.7 Local Deep Research（9.3k）—— 隐私与安全工程最硬
**架构**：FastAPI + LangGraph agent；Web UI + REST + WebSocket + MCP server；**每用户 SQLCipher 加密库（AES-256）**；容器 `cap_drop: ALL`、非 root、Cosign/SLSA/SBOM、egress 策略。
**真强项**：① 学术源最广：arXiv/PubMed/Semantic Scholar/Wikipedia/SearXNG/GitHub/Elasticsearch/Wayback/Guardian/Wikinews；② **OpenAlex/DOAJ/掠夺性期刊检测**（约 28 万来源）；③ **公开 benchmark**：Qwen3.6-27B 在单张 3090 上 SimpleQA 95.7%；④ 全本地模型支持。
**硬伤**：Web 形态非桌面；无学习闭环；无多模态材料；无 Anki；无文件编辑。
**对 Cortex 的威胁**：中低（形态不同）。但它的**学术质量信号（掠夺性期刊）+ 静态加密**是 Cortex 缺的。

### 2.8 AIPOCH Open-Science（5.2k）—— 科研工作台最完整
**架构**：TypeScript；Electron 桌面 + CLI + 浏览器工作区；Apache-2.0；v1.0.2（2026-10-22）。
**真强项**：① **执行**：Python/R/shell、持久 Notebook、SSH/Slurm；② **可追溯产物**：不可变版本 + 代码/输入/执行历史/环境证据/可再执行；③ **`.science` 可移植包**（对话分支+文件版本+Notebook 记录+核验证据）；④ 文献工作区：参考文献管理、筛文、PDF 批注、图表抽取、书目导出；⑤ 597 个技能 + 市场 + MedSkillAudit。
**硬伤**：**无学习闭环**（无 SRS/闪卡/测验）；装包 255MB；Python/R 环境是价值前提；无移动端；无 Anki。
**对 Cortex 的威胁**：中。若你要"复现论文/跑实验"，它更强；若你要"学懂并记住"，它没有。

### 2.9 OpenScience（4.1k）—— 执行与领域专家最深
**架构**：Bun + SolidJS 工作区 + Electron 桌面 + CLI；`@synsci/openscience`。
**真强项**：① **执行最全**：shell/Python/R/notebook + Modal/SSH/Slurm/PBS；② Lead agent + worker + 5 个领域专家 + 数百技能；③ **BioNeMo 10 个适配器**（Boltz-2/DiffDock/OpenFold…），产物哈希入库；④ 权限模型细（always/高风险/放行，网络按主机询问）；⑤ 公开 benchmark：Terminal-Bench Science 53/70、BiomniBench-DA 82.2。
**硬伤**：首次运行需登录（headless 除外）；无学习闭环；无多端；无 Anki；重。
**对 Cortex 的威胁**：低-中（用户画像不同：计算型科研 vs 学习型）。

---

## 3. 六层深度对比

### 3.1 数据层（输入与库）

| 维度 | 谁最强 | Cortex 位置 |
|---|---|---|
| 输入格式广度 | **SurfSense**（扫描 PDF/OCR 中日英、Office、录音、图片） | 强：PDF/Office/网页/YouTube/录音 + Docling 版面解析 + poppler 兜底 |
| 学术检索源 | **LDR**（10+ 源 + 掠夺性期刊库） | 强：arXiv/PubMed/Semantic Scholar/Crossref/OpenAlex + SearXNG，五源并发 RRF |
| 库的结构 | **AIPOCH**（文献工作区 + 不可变产物） | 独有：学科/主题/来源/分块 + 字符偏移 + 论文库 + 笔记 Markdown 化 |
| 文献管理 | **AIPOCH**（筛文/批注/图表抽取/书目） | 中上：Zotero 链接、papers 表、引用键、摘录回链；无书目格式导出细节 |

### 3.2 检索与接地（citation / evidence）

| 维度 | 谁最强 | Cortex 位置 |
|---|---|---|
| 证据精度 | **PaperQA2**（LitQA2 超人类宣称 + RCS） | 中上：引用字符偏移、零检索红边、显式检索范围三态、中文检索修复 |
| 引用核验 | **PaperQA2**（页码级）/ **LDR**（规范引用） | 强：`citecheck` 剔除幽灵引用（deep research 的 [n] 校验） |
| 元数据校验 | **PaperQA2**（Crossref/S2/Unpaywall + 撤稿） | 部分：Crossref/OpenAlex/S2 元数据；**无撤稿检查、无 Unpaywall 全文** |
| 学术质量信号 | **LDR**（DOAJ + 掠夺性期刊） | 缺 |
| 矛盾/分歧识别 | **PaperQA2**（contradiction，benchmark 支撑） | 有产品化形态：跨论文张力（带 verbatim 引用 + 严重度 + 导出） |
| 溯源透明度 | **AIPOCH/OpenScience**（执行证据链） | 独有形态：孤证警告（单源论断降级）、工具调用参数可见、提案 diff |

### 3.3 生成层

| 维度 | 谁最强 | Cortex 位置 |
|---|---|---|
| 长报告 | **STORM / GPT Researcher**（2000+ 字、带引用） | 有：deep research 五步（发现→筛选→入库→综述初稿→引用校验→存笔记） |
| 学习材料 | **Cortex**（9+ 种：速查表/闪卡/完形/测验/播客/幻灯片/信息图/导图/视频/双语精读/术语表） | **第一**。SurfSense 有播客/卡片/导图/测验，但没有速查表/幻灯片/信息图/视频/双语 |
| 文件产出 | **SurfSense**（改 Word/Excel/PPT，修订与批注） | 中：Office 生成 + docx 局部编辑（apply_edits）；无修订/批注、无表单填充 |
| 图像/视频 | GPT Researcher（内联配图） | 有：信息图、视频概览、确定性 SVG 图表（可复现） |

### 3.4 执行层（compute）

| 维度 | 谁最强 | Cortex |
|---|---|---|
| Python/R/notebook | **OpenScience / AIPOCH** | **无**（明确不做） |
| 远程集群 | **OpenScience**（Modal/SSH/Slurm/PBS）、AIPOCH（SSH/Slurm） | **无**（homelab 只做转录/同步） |
| 计算产物溯源 | **AIPOCH**（代码+输入+环境+可再执行） | 不适用（不做计算） |

> 这是**结构性差距**，也是路线图明确"不做"的一层。自用场景下，真要跑实验就用 Jupyter/IDE，不必让学习工具背这个包袱。

### 3.5 学习闭环

| 维度 | 谁有 | Cortex |
|---|---|---|
| 间隔重复（FSRS/SRS） | **只有 Cortex**（9 家全无） | ✅ 闪卡 + 完形 + SM-2/FSRS 排期 |
| 掌握度画像 | 无 | ✅ 学习者画像（基于真实掌握数据推理） |
| 陪练/脚手架 | 无 | ✅ 陪练台（脚手架可升可降） |
| 考试节奏 | 无 | ✅ 倒计时 Banner（7 天橙 / 3 天红）+ 自动闪卡 |
| 英语/写作训练 | 无 | ✅ 六种精读 + 翻译训练 + 写作工具箱（从采纳的修改中生长） |
| 复习→产出回流 | 无 | ✅ 摘录/片段→复习题→MOC 地图（笔记也计入覆盖） |

> 这一层是 Cortex **唯一 9:0 的完胜区**，也是"学习 OS"这个定位的真实支撑。

### 3.6 生态与工程

| 维度 | 谁最强 | Cortex |
|---|---|---|
| MCP | **GPT Researcher**（client + server + Skill） | **双向**：server（论文库/笔记上桌）+ client 模式 + 工具分级 + 写权限 + 提案 diff |
| 多端 | **Khoj**（6 端） | 桌面 + iOS 录音 + live sync；无浏览器/手机聊天端 |
| 技能生态 | **AIPOCH**（597 + 市场）/ OpenScience（数百） | 4 个内置模板，无市场（自用不需要） |
| 静态加密 | **LDR**（SQLCipher 每用户） | 无（仅备份加密：age + rclone） |
| 可移植 | **AIPOCH**（.science）/ Cortex（.cortex-pkg） | ✅ 归档 + Markdown 库（SQLite 只做索引） |
| 工程严谨 | OpenScience/AIPOCH（benchmark）/ Cortex（CI -D warnings + 测试） | ✅ CI 严格门 + 确定性 SVG + 离线兜底 |
| 成熟度 | Khoj/GPT Researcher（数万星、文档全） | 单人自用项目；功能密度高但边缘情况靠自测 |

---

## 4. Cortex 的独占组合（不是单点）

9 个项目里没有任何一个同时具备以下**任意两条**：

1. **学习闭环 × 多模态生成**：生成的材料直接进 FSRS 复习队列，复习结果反哺掌握度画像与陪练难度。SurfSense 能生成卡片，但没有排期、没有掌握度、没有回流。
2. **系统综述工作流 × 证据链**：PRISMA 8 维 + 每值 verbatim 证据 + 摘录回链 + Excel 导出；跨论文张力必须带原文引用否则拒收；孤证自动降级。PaperQA 的证据更强，但没有这套评审级工作流。
3. **Agent 写回 × 人类裁决**：agent 只能提提案（带 diff），人批准才落库；工具按 tier 授权，写工具单独一档。9 家都没有"提案制"。
4. **Anki 双向**：`.apkg` 导入导出 + 无头迁移脚本。9 家全无。
5. **单机一体化**：库 → 材料 → 复习 → 科研 → 录音 → 备份，全在一个桌面应用里，不依赖账号与云。

> 一句话：**别人各自把一层做到极深，Cortex 把六层串成一个闭环。** 这是"学习 OS"与"科研工作台/研究代理"的根本区别。

---

## 5. 诚实的短板

### A. 故意不做（不做也说得通）
- Python/R/notebook、SSH/Slurm/PBS、科学计算适配器 —— 执行层让给 IDE/Jupyter/OpenScience。
- 多端（浏览器/手机聊天）、Channels、技能市场、公开 benchmark —— 分发与社区需求，自用不需要。

### B. 真该补（自用有感，成本可控）
| # | 缺口 | 对手参照 | 为什么自用值得 | 量级 |
|---|---|---|---|---|
| 1 | **撤稿/勘误校验** | PaperQA2 | 综述引用一篇已撤稿论文是硬伤；入库时查 Crossref 的 `update-to` 关系即可 | 小 |
| 2 | **Unpaywall OA 全文** | PaperQA2 | 搜到论文直接拿开放获取 PDF 入库，省手动下载 | 小 |
| 3 | **期刊质量信号** | LDR（DOAJ/掠夺性期刊） | 筛文献时排除掠夺性期刊，自用也能少踩坑 | 中 |
| 4 | **小型自评集** | LDR/OpenScience 的 benchmark | 用自己领域的 30–50 道题验证检索质量，比公开跑分有用 | 小 |
| 5 | **静态加密（可选）** | LDR（SQLCipher） | 笔记本丢失时的兜底；全盘加密可替代 | 中 |

---

## 6. 对"自用"的取舍建议

**值得学（按性价比排序）**
1. PaperQA 的**撤稿检查**——已有 Crossref，加一个字段与提示，成本最低、收益最直接。
2. **Unpaywall 全文**——把"搜到"变成"能读"，对文献工作流是质变。
3. **自评集**——30 道本领域问题，改检索参数后跑一遍，防止越改越差。

**不值得学**
- 执行层（Python/R/集群）：自用直接开 Jupyter。
- 多端与技能市场：一个人的使用场景不需要。
- 公开 benchmark：跑分是给别人的，自用只需要自己的自评集。
- 文件编辑广度（SurfSense 的修订/批注/表单）：自用场景低频，真要做文档有 Office/WPS。

---

## 7. 结论

- **形态上**：SurfSense 是唯一同形态对手（本地桌面 + NotebookLM 叙事），差异在"学完记得住"与"研究做得深"。
- **单点上**：PaperQA2（证据）、LDR（隐私/学术源）、AIPOCH/OpenScience（执行与溯源）、Khoj（多端）、STORM/GPT Researcher（长报告）各有一层比 Cortex 强。
- **组合上**：**"学习闭环 × 多模态生成 × 科研证据链 × 单机一体化"四维交叉，9 家里没有第二家。**
- **自用结论**：不缺功能，缺的是三件小而关键的补丁——**撤稿校验、OA 全文、自评集**。补完就可以安心用起来，不必再追对手的执行层与多端。
