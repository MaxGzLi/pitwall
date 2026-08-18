# @maxgzli/dsh-monitor

把本机的 **agent-monitor** 守护进程接进 DeepSeek Harness：左下角常驻一个 Agent Dock，
模型侧多两个只读工具，可以直接在 DSH 里问「现在有几个 agent 在跑」「今天让 agent 做了什么」。

插件只读。它对守护进程只发 `GET`，不写 `~/.claude` / `~/.codex` / `~/.dsh`，
也不碰 herdr 的任何 mutating 方法。

## 组成

一个 npm 包，两个互不共享内存的半边：

| 半边 | 入口 | 运行位置 | 职责 |
|------|------|----------|------|
| Host | `main` → `lib/index.js` | dsh 的 node 进程 | 用 Node 22 全局 `fetch` 读守护进程；注册 RPC 通道与模型工具 |
| Client | `exports["./client"]` → `lib/client.js` | 浏览器 | `shell.overlay` 里的 Dock + `sidebar.footer.action` 里的入口按钮 |

两边只通过一个 **单向 unary RPC 通道** `/monitor-rpc` 通信（`authority: 'loopback'`）。
框架没有 server → client 推送，所以面板是轮询的。

浏览器半边**不会**直接访问守护进程：那是另一个 origin，守护进程按设计只放行 loopback CORS，
所有数据都走 Host 中转。

### 守护进程接口

Host 半边只用这三个只读端点（默认 `http://127.0.0.1:39917`）：

```text
GET /api/snapshot
GET /api/sessions?limit=&since_ms=
GET /api/summary/{harness}/{session_id}
```

`/api/sessions` 返回的是 `[[session, summary|null], …]`（Rust 侧 `Vec<(AgentRow, Option<SummaryRow>)>`
的序列化结果，是二元数组的数组，不是对象数组），插件在 `normalizeSessions()` 里做归一化。

## 模型工具

两个工具都包在 `if (config.toolsEnabled)` 里。**关掉它，普通开发 Session 就一个 schema token 都不用付**，
Dock 和 RPC 仍然可用。

### `monitor_status`

无参数。回答「现在有几个 agent 在跑 / 还剩多少额度」。

```json
{
  "available": true,
  "generatedAt": "2026-08-18T07:19:30.048Z",
  "day": "2026-08-18",
  "live": 8,
  "running": 3,
  "byState": { "working": 3, "idle": 5 },
  "quota": [{ "provider": "anthropic", "window": "7d", "usedPercent": 85, "plan": "Max", "resetsInMinutes": 401 }],
  "agents": [{ "harness": "claude", "project": "agent-monitor", "title": "◑ Agent监控服务与DSH插件",
               "state": "working", "turns": 5, "tokens": 6331327, "costUsd": 7.3658, "idleMinutes": 0 }],
  "todayCostUsd": 499.0396,
  "todayTokens": 581034384
}
```

Agent 列表截到 12 条，标题按 `maxTitleChars` 截断，`cwd` 和所有 `*_at_ms` 原始字段都不下发。

### `monitor_sessions`

参数 `{ limit?, sinceHours?, harness? }`，返回最近的会话**连同守护进程已经存好的总结**。
总结是守护进程离线算好的，这个工具不会触发新的总结调用。

```json
{
  "available": true, "count": 20, "withSummary": 1, "sinceHours": 48,
  "sessions": [{
    "harness": "codex", "project": "dsh", "state": "idle",
    "turns": 1, "tokens": 4130831, "costUsd": 3.1543, "durationMinutes": 9,
    "summary": { "headline": "实现 DSH monitor 插件的宿主半边与两个模型工具",
                 "body": "…按 maxSummaryChars 截断，截断处补 …",
                 "model": "deepseek-v4-flash", "status": "ok" }
  }]
}
```

守护进程没起来时两个工具都不抛异常，而是返回 `{"available": false, "reason": …, "hint": …}`，
让模型能直接告诉你「监控没在跑」。

## 安装到 DSH web profile

> ⚠️ 如果 `dsh` 不在 PATH 上（CLI 只以包文件形式存在），把下面的 `dsh` 换成
> `node ~/.dsh/profiles/node_modules/@deepseek-ai/dsh/lib/bin.js`。

```bash
# 1. 先构建（tsc 出 host 半边 + tsdown 出 lib/client.js）
cd plugin
corepack pnpm install
corepack pnpm run check          # build + test

# 2. 加进 web profile（开发目录联调，会写成 link: 依赖）
dsh plugin --profile web add ./plugin

# 3. 确认解析结果里有 dsh-monitor
dsh --profile web --dump-config

# 4. 起 DSH
dsh --profile web
```

这一步会做两件事：把 `@maxgzli/dsh-monitor` 写进 `~/.dsh/profiles/web/package.json` 的
`dependencies` 与 `dsh.profile.bundles`，并让 loader 应用本包的
[`cordis.patch.yml`](./cordis.patch.yml) —— 也就是那条 `insert` 的 `dsh-monitor` 行。

### 改配置

**不要**改本包的 `cordis.patch.yml`（那是 bundle 层的默认值）。
改 `~/.dsh/profiles/web/cordis.patch.yml`，用 `id` 定位，和现有的 `dsh-brain` 一段并列：

```yaml
- id: dsh-monitor
  config:
    serviceUrl: 'http://127.0.0.1:39917'
    toolsEnabled: true         # false = 模型侧完全看不到这两个工具
    requestTimeoutMs: 4000
    pollIntervalMs: 5000       # Dock 轮询间隔
    defaultSessionLimit: 10
    maxSessionLimit: 30
    maxSummaryChars: 600       # 单条总结正文给模型的字符预算
    maxTitleChars: 90
```

一条 patch **整体替换**该行的 `config` 对象，不做深合并 —— 要写就把上面这些字段都写全。

`serviceUrl` 只接受 loopback（`127.0.0.1` / `localhost` / `::1`）上的 `http://`，
配错会在插件加载时直接报错，而不是安静地把请求发到外网。
也可以用环境变量 `AGENT_MONITOR_URL` 覆盖。

### 改完代码要不要重启

| 改动 | 是否需要重启 dsh |
|------|------------------|
| `src/client/**`（重跑 `pnpm build` 后） | **不需要**。`@deepseek-ai/dsh-client-hmr` 在 host 侧 stat-poll bundle 的 mtime，变了就用 SSE 推一次 reload，浏览器自己刷新 |
| `src/*.ts`（host 半边） | **需要**。host 插件跑在 dsh 的 node 进程里，得重启 |
| `~/.dsh/profiles/web/cordis.patch.yml` | **需要** |

## 界面

- **Dock**（`shell.overlay`，`id: dsh-monitor`，`order: 30`）：右下角一颗胶囊，显示「几个在跑」+ 最紧张的那条额度百分比；
  点开展开成最近会话列表，带各自的总结。位置在 `bottom: 80px`，刻意让开 `@maxgzli/dsh-brain` 那个 `bottom: 22px` 的 Dock，
  两个插件可以装在同一个 profile 里。
- **侧栏按钮**（`sidebar.footer.action`，`order: 30`）：点一下展开 Dock。

`shell.overlay` 这一层默认 click-through，只有 Dock 自己 `pointer-events: auto`，不会挡住底下的应用。

## 开发

```bash
corepack pnpm run build     # tsc + tsdown
corepack pnpm test          # vitest
corepack pnpm run check     # 两个都跑
corepack pnpm run dev       # tsc --watch（host 半边）
```

测试覆盖纯逻辑部分：payload 裁剪、`/api/sessions` 二元组归一化、工具参数校验、config 校验、
RPC 端点分派、Dock 的格式化函数。`tests/fixtures/*.json` 是从本机真实跑着的
`agent-monitord` 上抓的原样响应，不是手写的假形状。
