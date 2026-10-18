# Cortex 提升改进计划（2026-10-18 校准版）

> 依据：竞品调研报告（17 项目）+ 知源对照长文 + **对当前代码树的实测核对**。
> 本文档与两份材料的最大差异：**报告里有一部分缺口已经在这一轮迭代中关闭了**，
> 第 1 节先做事实校准，第 2 节只列真正还开着的口子，第 3 节给出分批执行计划。

---

## 1. 现状校准（先清历史欠账，避免重复投入）

### 1.1 报告中已过时的条目 —— 不要再做

| 报告/长文中的说法 | 实际状态 | 证据 |
|---|---|---|
| 「MCP 服务端暴露 ❌ / 只是内部 MCP 不是外部入口」 | **已关闭**：`cortex-mcp` stdio 服务端，13 个只读工具，协议合规（inputSchema / text content / isError / -32601），只读连接，零路径泄露 | `974970a`、`src-tauri/src/mcp_server.rs`（11 单测 + 进程级冒烟） |
| 「RFC-5545 ICS 导出未见」 | **已关闭**：`ics.rs` + `export_calendar_ics` + CalendarView 按钮 | `17cc566` |
| 「tauri-build.log 有 calamine 未导入 / 大量 unused_mut」 | **已关闭**：编译 0 警告 0 错误；12 个机械警告 cargo fix 清零。那份日志是旧状态 | `17cc566`、`743c3ce`；当前 `cargo check --all-targets` 输出 0 |
| 「dist/ node_modules 进了 git」 | **不成立**：`.gitignore` 全覆盖，`git ls-files` 零违规 | 审计完成 |
| 「多会话要确认入口」 | **已存在**：`chat_threads` 表 + `new_chat`/`listChatThreads`/`openChatThread` + ChatPanel 历史入口 | 后端与 UI 均在 |
| 「失败回归测试缺失」 | **部分关闭**：ICS 折行/转义测试抓出两个真 bug；MCP 的 `unknown_tool`/`unknown_method`/通配符转义/越界分页都是失败路径测试 | `17cc566`、`974970a` |

### 1.2 科研一体化六环节（此前评估）

找 ✅ / 读 ✅（PDF 文本层选区摘录带页码）/ 笔 ✅ / 引 ✅（Zotero 直连）/ 写 ✅（多章 + LaTeX）/ 实验 ✅（轻量卡片）。
**结论：科研主线不再有结构性空洞，剩余都是深度问题。**

### 1.3 已确认仍然存在的真实缺口（实测）

| 缺口 | 实测证据 | 性质 |
|---|---|---|
| 可移植归档 `.cortex-pkg` | `backup.rs` 只有 age+rclone 云端；无 tar/sha256 | **P0，真空白** |
| Studio 确定性 fallback | `guard_offline_llm` 直接 `Err(OFFLINE_MSG)` | **P0，真空白** |
| 多 Agent 共享上下文 | `agents.rs` 有 execute runs；无同对话切换 | **P0，起步** |
| 笔记双链 `[[...]]` + 反向链接 | `NotesView` 无解析；`obsidian.rs` 只单向导出 wikilink | **P1，真空白** |
| XMind 导出 | 全仓无 `xmind` | **P1，真空白** |
| 会话级检索率（零检索红框 + 环形图） | ChatPanel 无 retrieval 统计 | **P1，真空白** |
| 引用字符偏移 | chunks 表只有 `ord`/`loc`，无 char range | **P1，缺 schema** |
| 中英对照精读 | LiteratureReader 单栏 | **P1，真空白** |
| 检索 scope 三态 all/selected/none | scope 四档（subject/topic/source/sources），无显式"不检索" | **P2** |
| Notebook 删除恢复目录 `.deleted-<id>` | 直接物理删 | **P2** |
| PRISMA 8 维抽取 | MatrixView 是任意两两对比 | **P2** |
| AI 严厉度/人格 | 无 | **P2，1 天量级** |
| 划词"高亮→双链标记" | 有摘录/翻译/解释，无高亮锚点 | **P2** |
| CI verification gates | 无 `npm run verify` 全套 | **P2** |

---

## 2. 分批执行计划

### 批次一（约 1.5 周）——「可移植性 + 信任感」，全是高 ROI 小件

**✅ 已完成（2026-10-18 晚）**：归档 `b2597db/06d2354` · 检索率 `3afadd6` · 双链 `d84d10f` · XMind `67a1a43` · 语气 `16abae9`

#### 1.1 可移植归档 `.cortex-pkg.tar.gz`（P0 #2，2–3 天）
**目标**：一个能 grep、能 diff、能单机迁移的快照；与云端备份并存（不是替代）。

- `pkg.rs`：打包 = `manifest.json`（版本、导出时间、文件清单+每文件 SHA-256、源数量）+ `cortex.db`（含 WAL checkpoint）+ `attachments/`（按 source_id 命名，不暴露原路径）。
- **拒绝孤儿**：DB 里引用的文件缺失 → 列出并中止（`--force` 可跳过，跳过项写进 manifest 的 `orphans`）。
- 导入 = 校验每个文件哈希 → 拒绝不匹配 → 解压到新目录 → **强制向量重建**（不复用旧 embedding，防跨身份投影）→ 通过 `CORTEX_DATA_DIR` 起隔离实例验证。
- UI：设置 → 备份区加「导出归档 / 导入归档」；CLI `cortex-pkg export|verify|import`。
- **验收**：导出→删除源库→导入→数据逐表一致（行数+抽样内容）；篡改一个字节 → 导入拒绝并指出文件。

#### 1.2 会话级检索率 + 零检索红框（P1，0.5–1 天）
**目标**：把「这段对话里多少回答真的查过资料」变成可见指标。

- 每条 assistant 消息持久化 `retrieval: {called: bool, hits: int, scopes: [...]}`（chat 消息表加 JSON 列，向后兼容）。
- ChatPanel 顶部：环形图（检索过/未检索）+ 计数；未检索回答加红色左边框 + 小标签「未检索」。
- **验收**：关闭检索问 3 个问题 → 3 条全红、环图 0%；开启后问 → 环图与红框同步变化。

#### 1.3 笔记双链 `[[...]]` + 反向链接（P1，1.5–2 天）
**目标**：`[[德布罗意波长|物质波]]` 在笔记里成链，被引用处可反查。

- 后端：`note_links` 表（from_note_id, target_slug, alias）+ 解析器（保存时增量重建该笔记的出链）+ `note_backlinks(slug)`。
- 前端：NotesView 渲染时把 `[[...]]` 变成链接（悬停显示目标标题）；右侧「反向链接」面板（谁引用了我）；未创建的链接点击可「新建同名笔记」。
- **验收**：A 引用 B → B 的反向链接面板出现 A；改名/删除 A → 反向链接同步消失（解析器在保存与删除两处都跑）。

#### 1.4 XMind 导出（P1，0.5 天）
- `xmind.rs`：知识框架树 → `.xmind`（zip + `content.json`，XMind 8/2020+ 可开）；节点带 mastery 作为标签。
- 入口：知识框架视图工具栏「导出 XMind」。
- **验收**：导出文件用 XMind/FreeMind 打开层级正确；节点数与框架一致。

#### 1.5 AI 严厉度（P2，1 天）
- 设置项 `coach_tone`: gentle/friendly/neutral/strict/savage；注入 chat + 学习材料 + Today 督促的 system prompt。
- **验收**：同一问题五档语气可辨；默认 neutral 不改变现有输出。

### 批次二（约 2 周）——「证据可验证」深一层 + 精读

**✅ 已完成（2026-10-19 凌晨）**:确定性 fallback `5ce6549`(P0 #3) · 恢复目录 `4bc56b4` · 字符偏移 `8e19573` · scope 三态 `945898c` · 中英对照 `459ab06`

#### 2.1 Studio 确定性 fallback（P0 #3，2–3 天）
**目标**：LLM 不可用/输出不合法时，产出**可用的确定性产物**，而不是报错。

- 新增 `fallback.rs`：给定 source/topic 文本 → 确定性产出
  - 章节大纲（标题层级/编号启发式）
  - 关键引用（高频句 + 定义句模式）
  - 术语表（中文术语+括号英文模式、高频名词短语）
  - 学习要点（每节首句 + 转折句）
- 生成链路：先试 LLM → schema 校验失败或 offline → 落 fallback 产物并**明确标注「确定性产物（未用模型）」**。
- **验收**：断网 + 无 key 下，cheatsheet/quiz 仍能生成并标注；有 key 时行为不变。

#### 2.2 引用字符偏移（P1，2–3 天）
- chunks 加 `char_start/char_end`（迁移 + ingest 时按在源文本中的实际位置写入）；历史数据用「顺序定位」回填脚本（chunk 文本在 content 中从上次结束位置 index_of）。
- MCP `search_notebook` 返回 `char_start/char_end`；聊天 evidence 面板显示 `p.14 · chars 3821-4310`。
- **验收**：任取一条 passage，用返回的偏移切片 == passage 文本（单测 + 真实库抽样）。

#### 2.3 检索 scope 三态（P2，1 天）
- ChatPanel scope 加显式「不检索」档；检索档位在消息上留痕（与 1.2 的 retrieval 字段复用）。
- 从 selected 升 all 需要显式确认（防静默扩大范围）。

#### 2.4 中英对照精读（P1，1–2 天）
- LiteratureReader 双栏：左原文段落、右译文（按段落懒翻译 + 缓存）；页码跟随滚动。
- **验收**：打开英文论文 → 两栏逐段对齐；滚动同步；切换单栏不丢位置。

#### 2.5 Notebook 恢复目录 + 删除提示（P2，1 天）
- 删除 subject → 移到 `data_dir/.deleted-<id>/`（含 DB 行快照 + 附件），UI 提示「已移到回收目录，可导出归档后手动清理」。
- **验收**：删除后数据可恢复（把目录移回并重新导入）；7 天后可在设置里一键清理。

### 批次三（约 2–3 周）——「Agent 平台 + 综述工作流」

**✅ 多 Agent 共享上下文已完成（`ea30a1b`,P0 #4)**：0047 thread_id + thread_messages(带 agent 归属)+ history_for_run(execute_run 读全线程) + agents_switch_agent + Agents UI 切换器/继续此线程/消息归属徽标。剩 PRISMA 8 维与 CI gates 未做。

#### 3.1 多 Agent 共享上下文（P0 #4，1.5–2 周）
- `agent_runs` 增加 `thread_id`：同一 thread 内切换 agent 时，把前序消息作为共享上下文注入（引用式，不复制）。
- UI：对话内 agent 切换器（课程助教 → 复习教练 → 文档助手），切换不丢上下文；每条消息标注由哪个 agent 产生。
- **验收**：切换 agent 后追问「刚才第三个要点是什么」能答对；上下文 token 不重复累积。

#### 3.2 PRISMA 8 维抽取（P2，1 周）
- 固定模板（研究问题/方法/贡献/结果/局限/适用人群/限制/复现成本）+ 逐篇抽取 + 证据句回链摘录 + 汇总表导出 CSV/Markdown。
- 与现有 MatrixView 并存：Matrix 是自由对比，PRISMA 是综述专用。

#### 3.3 CI verification gates（P2，2–3 天）
- `npm run verify` = svelte-check + vite build + `cargo check --all-targets` + `cargo test --lib` + `cargo clippy -- -D warnings`（先修 clippy 基线）。
- GitHub Actions：push/PR 触发；README 加状态徽章。
- 附带：README「能力边界表」（✅/🚧/❓ + boundary 列，仿 Gloss）。

---

## 3. 顺序与依赖理由

1. **先做批次一**：四个小件互相独立、都有明确验收，能在 1.5 周内把「可移植性」从空白变成卖点；MCP（已做）与之组合，外部 agent 既能读库也能拿归档迁移。
2. **再做批次二**：确定性 fallback 依赖生成链路统一入口（已有 `generate_material` 单点）；字符偏移要在 ingest 层一次性加好，晚做会积压历史回填成本。
3. **最后做批次三**：多 Agent 共享上下文是唯一的大件，且需要先有 retrieval 留痕（1.2）和 scope 三态（2.3）作为上下文边界，否则切换 agent 时检索范围会静默漂移。

## 4. 明确不做（战略判断）

- **不做** OpenScience 级 Notebook/SSH/Slurm 调度（已在 experiments.rs 注释与 README 中定位为「另一个产品」；补文档说明即可）。
- **不做** 校园速递/公众号订阅库（定位不是校园工具）。
- **不做** 逐帧桌宠动画（投入产出比低）。
- **不做** 完整 Notion 块编辑器（2–3 周换掉 Markdown 编辑器收益不确定；先把双链做出来，块编辑留作后续评估）。
- **不做** 6 类模型分别配置的 LiteLLM 层（当前按 task 分模型已够用，除非出现明确用户诉求）。

## 5. 验收总纲（每批次结束必须满足）

- `cargo test --lib` 全绿 + 新增功能至少 2 个失败路径测试（假成功必须报失败）。
- `cargo check --all-targets` 0 警告；`svelte-check` 0 错误；`vite build` 成功。
- 每个 P0/P1 功能有**一句话验收脚本**（见各条目「验收」）。
- 提交粒度：一个功能一个 commit，commit message 写清边界与测试。

---

## 6. 一页纸优先级总表

| 优先级 | 功能 | 工作量 | 批次 |
|---|---|---|---|
| P0 | 可移植归档 .cortex-pkg | 2–3 天 | 一 |
| P0 | Studio 确定性 fallback | 2–3 天 | 二 |
| P0 | 多 Agent 共享上下文 | 1.5–2 周 | 三 |
| P1 | 会话级检索率 + 零检索红框 | 0.5–1 天 | 一 |
| P1 | 笔记双链 + 反向链接 | 1.5–2 天 | 一 |
| P1 | XMind 导出 | 0.5 天 | 一 |
| P1 | 引用字符偏移 | 2–3 天 | 二 |
| P1 | 中英对照精读 | 1–2 天 | 二 |
| P2 | AI 严厉度 | 1 天 | 一 |
| P2 | 检索 scope 三态 | 1 天 | 二 |
| P2 | Notebook 恢复目录 | 1 天 | 二 |
| P2 | PRISMA 8 维 | 1 周 | 三 |
| P2 | CI gates + README 边界表 | 2–3 天 | 三 |

**关键路径**：批次一（1.5 周）→ 批次二（2 周）→ 批次三（2–3 周），总计约 6 周；若资源只够一个方向，按批次一全做（含已完成的 MCP，可对外讲「可移植 + 可接入」的故事）。
