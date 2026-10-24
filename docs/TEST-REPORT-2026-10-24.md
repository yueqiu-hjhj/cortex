# Cortex 全流程测试报告（2026-10-24）

> 版本 1.0.36（迁移 0090）。Windows x64，Rust 1.99，Node 22。
> 所有用例使用独立的内存 SQLite，并完整跑一遍全部迁移——测的是"新装用户第一次打开"的数据库状态。

## 1. 结果总览

| 检查项 | 命令 | 结果 |
|---|---|---|
| 单元 + 集成测试 | `cargo test --lib` | **667 passed / 0 failed / 7 ignored** |
| 跨模块端到端 | 内置 `flow_tests`（本轮新增） | **5 / 5 passed** |
| 静态检查 | `cargo clippy --lib -- -D warnings` | 干净 |
| 前端类型 | `svelte-check` | **0 错误**（11 个既有警告） |
| 前端构建 | `vite build` | 通过 |
| 安装包 | `tauri build --bundles nsis` | 成功（19.9 MB） |

7 个 ignored 全部是需要外部条件的用例，不是跳过缺陷：live OpenRouter、live vision、OpenAlex 实网、真实库往返（clips/moc/node_docs）、ffmpeg 下载编码。它们默认不跑，避免测试依赖网络与付费额度。

## 2. 跨模块端到端链路

单测证明每个模块自己是对的；这 5 条证明**模块之间的接线**是对的。

| 链路 | 覆盖 | 断言 | 结果 |
|---|---|---|---|
| 知识链 | 科目 → 来源 → 分块 → 关键词检索 → 笔记 | 检索能命中刚导入的块；笔记可列出 | ✅ |
| 计划链 | 目标 + 长期计划 → LLM 拆周 → 落库 → 排本周 | 第 1 周有块，且**不会把第 2 周任务拉进来** | ✅ |
| 智能体链 | 建两个 agent → 父委派子 → 子返回 → 父汇总 | 子 run 的 `parent_run_id` 指向父；父最终有回答 | ✅ |
| 技能链 | 建技能 → 导出文件 → 同名冲突 → 覆盖导入 | 冲突被拒；覆盖后 `source=imported`、正文一致 | ✅ |
| 复习链 | 任务 → 排块 → 完成块 → 关闭任务 → 校准 | 实际用时被记录；校准样本 +1 | ✅ |

## 3. 本轮发现并修复的缺陷

**委派之后，父 run 找不到自己**（严重度：高；只影响 `delegate`）

- **现象**：agent 调用 `delegate` 后，即使子任务成功，父 run 仍被标成失败，错误是 `NotFound("run …")`。
- **根因**：`execute_run` 收尾时用 `list_runs(limit=1)` 取"该用户最新一条"当作自己。子 run 的 `started_at` 更晚，于是"最新一条"变成了子 run，父 run 不在结果里。
- **修复**：新增 `agents::get_run(conn, user_id, id)` 按 id 直查（带归属校验），`execute_run` 改用它。
- **回归**：`flow_tests::agent_chain_runs_tools_and_delegates` 锁死这条路径——以后再有人改回"猜最新一条"，测试会红。

> 这正是端到端测试的价值：37 个 `agent_exec` 单测全绿，因为它们从不委派；只有跨模块的链路才踩得到。

## 4. 模块测试分布（前 20）

| 模块 | 测试数 | 模块 | 测试数 |
|---|---:|---|---:|
| repo | 42 | literature | 13 |
| agent_exec | 37 | tutor | 12 |
| office | 25 | papers | 12 |
| llm | 25 | node_docs | 12 |
| clips | 23 | workitems | 11 |
| planner | 21 | proposals | 11 |
| eval | 21 | prompt_lab | 11 |
| clip_server | 19 | note_knowledge | 11 |
| mcp_server | 18 | ingest | 11 |
| citesnap | 17 | growth | 11 |
| english_bench | 14 | commands | 11 |

其余约 45 个模块合计 301 个测试（vault 7、plan_build 7、provenance 7、cron 9、connectors 8、answer_health 8、agents 8……）。

## 5. 无法自动化的部分

这些必须走 UI 或真实模型，自动化成本高于收益：

1. **Tauri 命令层**：`chat_answer`、材料生成、同步等需要 `AppHandle`，单测覆盖的是它们调用的服务函数。
2. **真实模型输出质量**：拆解是否合理、综述是否有洞见——不是断言能判的。
3. **UI 交互**：拖拽排期、文件选择对话框、富文本编辑。
4. **外部服务**：SearXNG、OpenAlex、Moodle、Zotero、Google 日历。

### 手工验收清单（建议按顺序点一遍，约 15 分钟）

- [ ] 新建科目 → 导入一份 PDF → 看到分块数 > 0
- [ ] 问一个只有该 PDF 能回答的问题 → 回答带引用，点引用能跳回原文
- [ ] 把聊天 scope 切到"不检索" → 出现「未检索」徽章
- [ ] 生成一份速查表 → 进「材料」能看到并导出
- [ ] 把生成的闪卡加入复习 → 「今天」出现卡片 → 评一次分
- [ ] 建目标 → 粘贴 4 周计划 → 拆成周任务 → 选第 1 周排期
- [ ] 智能体：建「知识库管家」→ 问一句 → 看工具步骤
- [ ] 建两个 agent → 父 agent 提示词要求用 delegate → 运行列表出现「↳ 子任务」
- [ ] 技能：导出一个技能 → 再导入 → 弹同名确认
- [ ] 设置里填命令白名单（如 `cargo test`）→ 代码助手跑一条命令

## 6. 结论

- 核心链路（知识 → 检索 → 生成 → 复习 → 计划 → 智能体）在全新数据库上全部走通。
- 本轮修复了 1 个高危缺陷（委派收尾），并把它锁进回归测试。
- 剩余风险集中在**真实模型质量**与**外部服务可用性**，这两类只能靠使用中观察，不适合自动化断言。
