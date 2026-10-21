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

## 3. 工具面（18 个，全部只读）

**科目与资料库**
`list_notebooks` · `search_notebook` · `get_source` · `read_document`

**笔记**
`search_notes` · `get_note`

**论文（新增）**
`list_papers` · `get_paper` · `search_papers` · `list_excerpts`

**知识结构**
`get_knowledge_tree` · `list_wiki` · `get_wiki` · `wiki_reverse_links` · `cross_book_bridges` · `list_provenance`

**agent 运行记录**
`list_agents` · `agent_messages`

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

`get_note` 返回正文、vault 路径、出链与反链。写回目前**不由 agent 直接完成**——
agent 起草，你在 Cortex 里确认后落盘（提案队列，P3）。

**论文管理与探索**
> 「比较我收藏里 BBR 和 CUBIC 的实测结论，只用摘录里的句子，标出页码。」
> 「围绕 attention 这个主题，我收藏的论文里有哪些互相矛盾的结论？」

`get_paper` 给的是**你自己的摘录**，不是摘要——摘录带页码，是真正能写进论文的证据。

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

- **连接是 SQLITE_OPEN_READ_ONLY**。MCP 进程在任何情况下都不会写入你的库。
- **不调用任何模型**。这里返回的是你已有的内容，不含生成。
- **不暴露文件系统绝对路径**（`vault_path` 是 vault 内的相对路径）。
- 笔记的写入走 Cortex 应用本身（或你直接编辑 `.md`），不经过 MCP。

---

## 7. 排障

**`数据库不存在`**
先打开一次 Cortex 桌面应用（迁移要跑一次），或用 `--db` 指定已有库。

**工具列表里没有 `list_papers`**
客户端缓存了旧的 `tools/list`。重启客户端；或确认 `cortex-mcp --help` 显示 18 个工具。

**`no paper with cite_key ...`**
库里的 `cite_key` 由主应用启动时回填（迁移 0059 之后）。打开一次 Cortex，
或先调 `list_papers` 拿到实际的 key——它会为未回填的行给出派生值。

**中文标题/作者**
`cite_key` 保留中文字符（`张三2020认知负荷`）。BibTeX 用 biblatex 时正常，
传统 bibtex 对非 ASCII key 支持有限，必要时在导出后手动改。
