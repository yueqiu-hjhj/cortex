# cortex-paper-meta — 撤稿校验与开放全文

补上深度对比里标出的两个真实短板（`docs/competitive-deep-dive-2026-10-22.md` §5B）：

| 短板 | 对手参照 | 本工具 |
|---|---|---|
| 不知道引用的论文是否已撤稿 | PaperQA2（Crossref 元数据 + 撤稿检查） | `--check` 查 Crossref |
| 搜到论文却拿不到合法全文 | PaperQA2（Unpaywall） | `--fetch-oa` 下载 OA PDF |

## 为什么是独立工具而不是入库时检查

撤稿与 OA 状态都是**外部、批量、会变**的事实。放进导入流程会有三个问题：
导入被网络拖慢、离线导入失败、同一篇论文反复查。所以它们单独跑、结果落表，
论文本身（`papers`）保持可离线导入。

## 用法

```bash
# 1. 批量查：Crossref（撤稿）+ Unpaywall（开放获取）
cargo run --bin cortex-paper-meta -- --check

# 2. 下载已发现的开放全文（默认 ~/Downloads/cortex-oa）
cargo run --bin cortex-paper-meta -- --fetch-oa

# 3. 看结果
cargo run --bin cortex-paper-meta -- --report
```

编译后也可以直接跑二进制：

```bash
./src-tauri/target/debug/cortex-paper-meta --check --json
```

### 选项

| 参数 | 说明 |
|---|---|
| `--db <path>` | 指定 `cortex.db`；默认与应用一致（`--db` → `$CORTEX_DB` → `$CORTEX_DATA_DIR` → 应用数据目录） |
| `--email <mail>` | Unpaywall 要求的联系邮箱（默认 `$UNPAYWALL_EMAIL`，再退到 `cortex@localhost`）。建议设成真实邮箱，遵守他们的礼貌使用约定 |
| `--out <dir>` | `--fetch-oa` 输出目录 |
| `--paper <id>` | 只处理一篇 |
| `--limit <n>` | 最多检查 n 篇（试跑用） |
| `--force` | 忽略 30 天缓存强制重查 |
| `--json` | 机器可读输出 |

## 结果写在哪

新建表 `paper_quality`（工具自己 `CREATE TABLE IF NOT EXISTS`，**不改 `db.rs` 的迁移数组**）：

| 列 | 含义 |
|---|---|
| `paper_id` | 对应 `papers.id` |
| `retracted` | 1 = 这篇论文已被撤稿 |
| `retraction_doi` | 撤稿声明的 DOI |
| `retraction_kind` | `retraction` / `correction` / `expression-of-concern` |
| `is_notice` | 1 = 这篇本身就是撤稿声明 |
| `oa_status` | `gold` / `green` / `hybrid` / `bronze` / `closed` |
| `oa_pdf_url` | 开放获取 PDF 直链 |
| `checked_at` | 上次检查时间（ms） |
| `error` | 单篇失败原因，不阻塞其他篇 |

## 撤稿是怎么判定的

Crossref 有三种暴露方式，全部做了防御性解析：

1. `message.updated-by[]` 里 `type == "retraction"` —— 最常见；
2. `message.relation["is-retracted-by"]` —— 较新的记录；
3. `message.type == "retraction"` —— 这篇本身是撤稿声明（记为 `is_notice`，**不**算"这篇被撤稿"）。

`correction`（勘误）只记 `retraction_kind`，不会误报为撤稿。

## 下载行为

- 只下载 `retracted = 0` 且 `oa_pdf_url` 非空的论文；
- 文件名校验后截断（`safe_name`），不会逃出输出目录；
- 校验文件头必须是 `%PDF`，落地页/HTML 会被判失败并清掉失效链接；
- 已存在的同名文件跳过；
- 不自动入库——把下载好的 PDF 拖进 Cortex 即可走正常的分块+嵌入流程。

## 建议节奏

撤稿是低频但重要的事实，建议每周跑一次：

```bash
cortex-paper-meta --check --limit 50
cortex-paper-meta --report
```

写综述/投稿前再跑一次 `--check --force`，确保引用的论文没有被撤。

## 验证

纯解析逻辑有单元测试（不打网络）：

```bash
cd src-tauri && cargo test --bin cortex-paper-meta
```

覆盖：`updated-by` 撤稿、`is-retracted-by` 关系、撤稿声明标记、勘误不误报、
Unpaywall PDF 直链、closed 无链接、DOI 前缀归一化、URL 编码、文件名防穿越。

## 已知边界

- **UI 还没接入**：结果在 `paper_quality` 表里，`--report` 可看；文献列表的"已撤稿"角标待并行会话的改动稳定后再加（避免同时改 `papers.rs` / 前端）。
- Crossref/Unpaywall 覆盖不全：没有 DOI 的论文查不了（工具会跳过并计数）。
- 查询失败不会丢数据：错误写进 `error`，下次 `--check` 自动重试。
