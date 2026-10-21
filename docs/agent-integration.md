# 让 agent 进入 Cortex（Codex / Claude Code / Cursor）

Cortex 对外的融合面只有两个，这是刻意的设计：**一条文件通道，一条 MCP 通道**。
不给每个客户端单独做一套接口，否则 Codex 能做的事 Claude Code 不能做，每加一个功能要改三遍。

| 通道 | 覆盖内容 | 谁在用 |
|---|---|---|
| `<vault>/*.md` | 笔记（双链、frontmatter、附件） | Obsidian、git、任何能读写文件的 agent |
| MCP server | 资料库分块检索、论文库、摘录、知识图谱、wiki | Codex、Claude Code、Cursor、任何 MCP 客户端 |

论文库和摘录**同时**存在于 SQLite 和 MCP 工具面里，因为它们是结构化查询（按状态、按年份、按摘录页码），
文件搜索做不好这件事。笔记反过来以文件为准，因为笔记的价值在于可被任意编辑器直接打开。

---

## 1. 先确认二进制在哪

```bash
# 开发构建
ls src-tauri/target/release/cortex-mcp.exe

# 安装版（NSIS 打包时 companion 二进制会剥成短名）
ls "$HOME/Desktop/Cortex/mcp.exe"
```

`--help` 可以确认版本与工具数：

```bash
cortex-mcp.exe --help
```

---

## 2. 配置

### Codex CLI（`~/.codex/config.toml`）

```toml
[mcp_servers.cortex]
command = "D:\\cortex-base\\src-tauri\\target\\release\\cortex-mcp.exe"
```

### Claude Code / Cursor / 其他 MCP 客户端（JSON）

```json
{
  "mcpServers": {
    "cortex": {
      "command": "D:\\cortex-base\\src-tauri\\target\\release\\cortex-mcp.exe"
    }
  }
}
```

### 指向别的数据库

默认读平台应用数据目录。要换一份库（做实验、读备份）：

```bash
cortex-mcp.exe --db D:\somewhere\cortex.db
# 或 CORTEX_DB=<file> / CORTEX_DATA_DIR=<dir>
```

---

## 3. 工具面（19 个只读 + 3 个提案）

**科目与资料库**
`list_notebooks` · `search_notebook` · `get_source` · `read_document`

**笔记**
`search_notes` · `get_note`

**论文**
`list_papers` · `get_paper` · `search_papers` · `list_excerpts`

**知识结构**
`get_knowledge_tree` · `list_wiki` · `get_wiki` · `wiki_reverse_links` · `cross_book_bridges` · `list_provenance`

**agent 运行记录**
`list_agents` · `agent_messages`

**摘录（未归位的片段，见 §4.6）**
`list_clips`

**写入（提案队列，见 §4.5）**
`propose_note` · `list_proposals` · `withdraw_proposal`

### Resources

笔记和论文同时以 URI 暴露，可以直接 `@` 引用而不用先花一次工具调用去找：

```
cortex://note/<note_id>
cortex://paper/<cite_key>
```

---

## 4. 能问什么

**知识检索**
> 「我库里关于 TCP 拥塞控制的材料，哪些段落提到 BBR 的延迟表现？给出原文和出处。」

`search_notebook` 返回的每段带 `source_id` / `chunk_id` 和字符偏移，
`read_document` 可以按偏移继续读上下文——引用是可核验的，不是模型编的。

**笔记的使用与生成**
> 「读一下《TCP 核心机制笔记》的出链和反链，把这周新增的三条笔记并进一个 MOC。」

`get_note` 返回正文、vault 路径、出链与反链。写整篇 MOC 时 agent 用 `propose_note`，
你在 Cortex 里看 diff 后决定是否采纳（见 §4.5）。

**论文管理与探索**
> 「比较我收藏里 BBR 和 CUBIC 的实测结论，只用摘录里的句子，标出页码。」
> 「围绕 attention 这个主题，我收藏的论文里有哪些互相矛盾的结论？」

`get_paper` 给的是**你自己的摘录**，不是摘要——摘录带页码，是真正能写进论文的证据。

---

## 4.5 写回：agent 起草，你确认

Agent **不能直接改笔记**。它只有一条写路径：

```
agent ──propose_note──▶ agent_proposals（pending）
                              │
           Cortex「笔记 → AI 提案」显示逐行 diff
                              │
                    你点「采纳」 ──▶ notes 表 + vault/<标题>.md
```

```text
propose_note(
  mode      = "new" | "append" | "replace",
  title     = 新笔记标题 / 目标笔记标题,
  note_id   = 追加或改写时的目标（优先于 title）,
  body      = new/replace 是完整正文；append 只写新增那一段,
  subject   = 学科名（新建时可选）,
  rationale = 一句话说明为什么值得写,
)
```

三个细节决定了这套东西敢用：

1. **`base_body` 快照**。提案排队时的正文被记下来；如果这期间你自己又改过这篇笔记，
   采纳会被拒绝，除非你在弹窗里选「仍然应用」。所以一份过期的草稿永远不可能悄悄覆盖你的修改。
2. **幂等**。同样的目标 + 同样的正文重复提交，返回同一条提案，不会在队列里堆重复项。
3. **append 优先**。`append` 只把新段落接在现有正文后面，比 `replace` 安全得多；
   `replace` 适合"重写这一篇"，用之前先看一眼 diff 里的 `-` 行。

采纳之后走的是和手动保存**同一条代码路径**：`[[双链]]` 索引重建、frontmatter 盖章、
`.md` 原子写入，全都一致。所以 agent 写的笔记在 Obsidian 里也一样能用。

用到的工具：

| 工具 | 作用 |
|---|---|
| `propose_note` | 入队一份草稿，返回 proposal id、改动行数、前几行 diff |
| `list_proposals` | 看队列里还有什么在等（避免重复起草） |
| `withdraw_proposal` | 撤回自己已经过时的草稿 |

---

## 4.6 摘录：你选中一段，它就有地方去

这是**你自己的**动作，不是 agent 的，但它决定了 agent 能看到什么。

在 Cortex 里选中任何一段文字 —— AI 的回答、笔记预览、PDF 正文 —— 会浮出一个小条：

```
        ┌──────────────┬────────┐
        │ ✚ 摘到笔记   │ 先存着 │
        └──────────────┴────────┘
```

- **摘到笔记**：选一篇已有笔记（默认首选你正在读的那篇），或者当场新建一篇。
  片段写成引用块，落在目标笔记的 `## 摘录` 标题下，带上出处：

  ```markdown
  > BBR 在 100 Mbps / 40 ms 的链路上维持近线速，同时把排队延迟压在 20 ms 以内。
  >
  > —— AI 回答 · deepseek
  ```

  这条规则是硬性的：**只往 `## 摘录` 那一节追加，永不改你写在上面的正文**。
  所以摘十次也不会把你的文章搅乱。

- **先存着**：不决定归属，直接进「笔记」页顶部的摘录收件箱。回头点「归位」再选目标。

### 为什么这对 agent 有用

`list_clips` 返回的就是收件箱里那些**你亲手指过一遍**的句子。它们比搜索命中更能说明
你现在在想什么 —— 搜索是你问的，摘录是你选的。

于是可以这样用：

> 「看看我最近摘的几条，如果都在讲拥塞控制，就帮我起一篇骨架笔记，把我摘的原句引进去。」

agent 会 `list_clips` → 发现主题聚在一起 → `propose_note` 起草 → 你在笔记页点「采纳」。
它引用的每个句子都是你自己挑过的，不是它编的。

| 工具 | 作用 |
|---|---|
| `list_clips` | 待整理片段（`note_id` 为空）+ 出处；`pending_only=false` 连已归位的一起给 |

---

## 5. 引用约定

`cite_key` 在论文导入时分配一次，之后**永不改变**（迁移 0059）。
改标题、改作者都不会让它变，所以已经写进手稿的 `\cite{Vaswani2017Attention}` 不会断。

```
Vaswani2017Attention        # 首作者姓氏 + 年份 + 标题首个实词
Vaswani2017Attention-2      # 冲突时加后缀
```

让 agent 引用时同时给出 `cite_key` 和页码，例如：

> BBR 在高带宽时延积下仍能维持近线速吞吐（Sarpkaya2026Evaluation, p.4）。

---

## 6. 边界

- **唯一的写入目标是 `agent_proposals` 表**。连接本身可写（WAL + busy_timeout，
  和桌面应用的保存互不阻塞），但代码里所有写操作都经由 `proposals` 模块，
  它不会碰 `notes`——草稿只有在你点「采纳」之后才变成笔记。
- **不调用任何模型**。这里返回的是你已有的内容，不含生成；提案文本由 agent 自己写。
- **不暴露文件系统绝对路径**（`vault_path` 是 vault 内的相对路径）。
- `clips`（选区摘录）对 agent 只读：它能看到你摘了什么，不能替你摘、也不能改摘录。
- 只读副本（比如拷一份 db 出来看）仍然支持全部 19 个读工具，写工具会明确报错。

---

## 7. 排障

**`数据库不存在`**
先打开一次 Cortex 桌面应用（迁移要跑一次），或用 `--db` 指定已有库。

**`这个库还没有提案队列`**
库还没升到迁移 0060。启动一次桌面应用即可。

**`提案队列需要可写连接`**
`cortex-mcp` 打开的是可写连接；报这个说明你把它指向了只读副本。

**`list_clips` 返回 "No clips waiting"**
收件箱是空的。先在 Cortex 里选中一段文字并点「先存着」。

**工具列表里没有 `list_papers`**
客户端缓存了旧的 `tools/list`。重启客户端；或确认 `cortex-mcp --help` 显示 21 个工具。

**`no paper with cite_key ...`**
库里的 `cite_key` 由主应用启动时回填（迁移 0059 之后）。打开一次 Cortex，
或先调 `list_papers` 拿到实际的 key——它会为未回填的行给出派生值。

**中文标题/作者**
`cite_key` 保留中文字符（`张三2020认知负荷`）。BibTeX 用 biblatex 时正常，
传统 bibtex 对非 ASCII key 支持有限，必要时在导出后手动改。
