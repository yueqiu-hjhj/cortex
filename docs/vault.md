# cortex-vault — 数据库静态加密

补上深度对比 §5B #5（LDR 用 SQLCipher，Cortex 之前只有备份加密）。

## 它解决什么

笔记本没开全盘加密、又要带出门时，`cortex.db` 里是你的全部学习数据。
`cortex-vault` 在**应用关闭后**把数据库加密成一个 `.vault` 文件，明文删除：

```bash
cortex-vault seal     # cortex.db → cortex.db.vault，删除明文
cortex-vault open     # cortex.db.vault → cortex.db，应用可以启动
cortex-vault status   # 看当前有哪些文件
```

## 用法

```bash
# 口令走环境变量
export CORTEX_VAULT_PASSPHRASE='你的口令'
cortex-vault seal

# 或从 stdin（避免口令进 shell 历史）
read -s PW; printf '%s' "$PW" | cortex-vault seal
unset PW

# 出门回来
read -s PW; printf '%s' "$PW" | cortex-vault open
```

| 参数 | 说明 |
|---|---|
| `--db <path>` | 指定 `cortex.db`；默认与应用一致（`--db` → `$CORTEX_DB` → `$CORTEX_DATA_DIR` → 应用数据目录） |
| `--force` | `seal` 覆盖已有 `.vault`；`open` 覆盖已有 `cortex.db` |
| `--remove-vault` | `open` 成功后删除 `.vault`（默认保留作备份） |

## 安全设计

| 机制 | 作用 |
|---|---|
| **先验证再删除** | `seal` 加密后立刻用同一口令解密、比对 SHA-256；不一致就中止，明文原样保留 |
| **拒绝在应用运行时加密** | 先做 `wal_checkpoint(TRUNCATE)`，占用中直接报错，绝不复制半写状态的库 |
| **原子写入** | 先写 `.tmp` + `fsync`，再 rename，断电不会留下半个文件 |
| **头部校验** | `open` 检查 SQLite 文件头，解出来不是数据库就拒绝覆盖 |
| **头部参与认证** | magic + salt + nonce 作为 AAD，篡改 salt/nonce 会**解密失败**而不是得到垃圾 |
| **不覆盖已有明文** | `open` 默认拒绝覆盖 `cortex.db`，必须显式 `--force` |

## 密码学

- **ChaCha20-Poly1305**（AEAD，密文带 16 字节认证标签）
- 密钥：**Argon2id**（19 MiB、t=2、p=1）从口令派生，16 字节随机 salt
- 12 字节随机 nonce，每次加密都重新生成（salt 与 nonce 都不同）
- 文件布局：`CORTEXVAULT1`(12) + salt(16) + nonce(12) + 密文+tag

## 和全盘加密的关系

**全盘加密（BitLocker / FileVault）仍然是首选**：它覆盖整个磁盘，包括
`cortex.db-wal`、临时文件、备份。`cortex-vault` 是给"磁盘没加密"的情况兜底，
并且只在应用关闭时有效——应用运行时数据库必须以明文形态打开。

两者可以一起用：平时全盘加密，出差前再 `seal` 一层。

## 口令丢了怎么办

**无法恢复。** Argon2id 派生密钥，没有后门。所以：

1. `seal` 之后立刻确认 `.vault` 文件存在；
2. 不要删掉 `open` 之前的任何备份；
3. 口令建议存进密码管理器，而不是只记在脑子里。

## 建议节奏

```bash
# 出差/带电脑出门前
cortex-vault seal

# 回到可信环境
cortex-vault open
cortex-vault status
```

不建议每天 seal/open：频繁操作只增加误操作风险，收益有限。

## 验证

```bash
cd src-tauri && cargo test --bin cortex-vault
```

6 个测试：往返还原字节一致、错误口令失败、篡改 salt 认证失败、非 vault 输入拒绝、
每次 seal 的 salt/nonce/密文都不同、`.vault` 与数据库同目录。
