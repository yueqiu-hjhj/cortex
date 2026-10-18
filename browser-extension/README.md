# Cortex Clipper (A7)

一键把正在读的网页存进 Cortex，不用再手动复制粘贴 URL。

- **工具栏弹窗**：选学科 → 存整页 / 只存划选。
- **右键菜单**：`Clip to Cortex`（整页）、`Clip selection to Cortex`（只存划选）。
  点完直接在图标上给出 ✓ / ! 反馈，不弹窗。
- 存下的条目落在所选学科下，`kind = web`，并自动带上 `clip` 标签，方便筛选。
- 抓不到正文的页面（纯 JS 渲染、付费墙、PDF）不会丢：有划选就存划选，否则至少存下链接，
  并给出提示，之后可以在 Cortex 里重新抓取。

## 安装（Chrome / Edge / Brave，未打包加载）

1. 启动 Cortex 桌面端（扩展需要它在运行）。
2. 浏览器打开 `chrome://extensions`（Edge 是 `edge://extensions`）。
3. 打开右上角 **开发者模式**。
4. 点 **加载已解压的扩展程序**，选择本目录 `browser-extension/`。
5. 把 Cortex Clipper 固定到工具栏。
6. 在扩展的「设置」里可以改端口和默认学科（默认 `127.0.0.1:7878`）。

## 它怎么工作

Cortex 运行时会在本机回环地址上开一个 HTTP 服务
（`src-tauri/src/clip_server.rs`，默认 `127.0.0.1:7878`，可用环境变量
`CORTEX_CLIP_PORT` 改）。扩展只跟这个地址通信：

| 方法 | 路径 | 用途 |
| --- | --- | --- |
| `GET` | `/health` | 探活 —— 弹窗打开时先确认 Cortex 在运行 |
| `GET` | `/version` | 版本 + 绑定端口 |
| `GET` | `/subjects` | 弹窗的学科下拉框 |
| `POST` | `/clip` | 抓取并入库，body：`{url, title?, selection?, selection_only?, subject_id?, topic_id?}` |

安全边界：**只绑定 127.0.0.1**，不监听局域网；没有令牌，因为能访问本机回环的
进程本来就已经是这台机器上的用户。

抓取在请求线程里同步做（最多 20 秒），答复发出后**再**在后台线程里分块 + 向量化 ——
ONNX 嵌入模型冷启动可能要几分钟，扩展不该等它。

## 手动验证

```powershell
# Cortex 运行期间：
curl http://127.0.0.1:7878/health
curl http://127.0.0.1:7878/subjects

curl -X POST http://127.0.0.1:7878/clip -H "Content-Type: application/json" `
  -d '{"url":"https://example.com","subject_id":"<某个学科 id>"}'
```

## 文件

```
manifest.json          MV3 清单
src/service_worker.js  右键菜单 + 图标徽标反馈
src/shared.js          端口/设置/JSON 调用（弹窗与设置页共用）
src/popup.*            工具栏弹窗
src/options.*          设置页
icons/*.png            16 / 48 / 128
```
