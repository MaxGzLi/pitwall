# 实测数据格式与坑

2026-08-18 在本机实测得出，每条都经过对抗性复核（复核推翻的错误结论已标 ⚠️）。
改动采集层之前先读这份文档 —— 里面每个 ⚠️ 都是已经踩过一次的坑。

---

## herdr 0.8.0

**传输**：Unix SOCK_STREAM，`~/.config/herdr/herdr.sock`（0600），换行分隔 JSON。无 HTTP、无 TCP、无 webhook。

```
请求  {"id":"<必填>","method":"<m>","params":{<必填>}}\n
成功  {"id":"<回显>","result":{"type":"<result_type>",...}}\n
错误  {"id":"","error":{"code":"<snake_case>","message":"..."}}\n
```

⚠️ **一个连接只答一个请求**。服务端答完即关；在同一连接上写第二个请求会被静默丢弃，永远不返回。每次 RPC 开新连接。例外是流式方法（`events.subscribe`）会保持连接。

**实时事件**：`events.subscribe` 是唯一的推送通道。插件 manifest 的 `[[events]]` 钩子只认 `on = "startup"`，送不出 agent 状态。

⚠️ **同一条流上有两种信封**，这是最容易翻车的地方：

```
{"data":{...,"type":"pane_agent_detected",...},"event":"pane_agent_detected"}   ← snake_case
{"data":{"agent":"claude","agent_status":"working","pane_id":"w5:p1"},"event":"pane.agent_status_changed"}  ← 点号，载荷更窄
```

必须把 `pane_agent_status_changed` 和 `pane.agent_status_changed` 当两个 case 处理。

⚠️ **订阅会重放一段陈旧积压**：订阅瞬间会先收到历史事件，且载荷可能带过期字段值（实测收到一个 `workspace_created` 带的是该 workspace 的旧名字）。正确顺序是先 `events.subscribe`、再 `session.snapshot`，以 snapshot 为准，重放事件只当排序提示。

⚠️ 不要全局订阅 `pane.updated` —— 它随终端标题每个字节变化触发，一个 Claude spinner 就能产出约 10 事件/秒/pane。

⚠️ `events.wait` 只实现了 `pane_agent_status_changed` 一种匹配，且 `agent_status` 必填。schema 里列出的其他变体服务端一律拒绝（`unsupported_event_wait_match`）。

**状态机**：`idle · working · blocked · done · unknown`

- `done` = 「后台跑完了但你没看见」。**你切到那个 tab 的瞬间它就塌回 `idle`**；API/CLI 的读取不会标记已看见，所以监控要抢在人手之前把它记下来 —— 这正是本项目最有价值的那个信号。
- `unknown` **不等于**完成。
- Claude/Codex 的状态是**屏幕抓取**推断的，不是 harness 上报的：Claude 的 `working` 靠 OSC 标题里的 spinner 字形正则（`claude.toml` 规则 `osc_title_working`，优先级 1100）。因此 `working→idle` 会抖，需要去抖约 2.5s。只有 `pi` 和 `hermes` 通过 `pane.report_agent` 上报真实状态。
- ⚠️ `claude.toml` 有 **12** 条规则而非 6 条，其中 `bash_permission_prompt`(850) 和 `generic_permission_prompt`(840) 产出 `blocked` —— 做审批检测时它们是关键。

**最有价值的一条**：`agent_session.value` 就是 harness 原生的 session id。对 Claude 而言即 `~/.claude/projects/<slug>/<uuid>.jsonl` 的那个 UUID（实测 5/5 命中）。herdr 本身**不存任何对话内容**，它的贡献是这个 `pane_id → session UUID` 的索引，而这个索引换别的途径很难拿到。

⚠️ `foreground_cwd` 不能用来判断项目归属 —— 它经常指向 MCP/插件目录。用 `cwd`。

**结束信号**：没有单一的 `agent_exited` 事件，分三种：

| 情况 | 信号 |
|---|---|
| 一轮跑完，agent 还活着 | `pane.agent_status_changed` → `idle` / `done` |
| agent 进程退出，pane 还在 | `pane_agent_detected` 带 `released:true`、`agent:null`、`final_status` |
| pane 本身死了 | `pane_exited` 然后 `pane_closed` |

⚠️ 其他修正：`server.agent_manifests` 返回 **19** 个而非 21 个；`~/.config/herdr/herdr.log` **不存在**（只有 `herdr-server.log` 和 `herdr-client.log`）；`pane.graphics.stream` 是真实方法，只是被 `experimental.kitty_graphics` 开关挡住。

---

## Claude Code

| 项 | 值 |
|---|---|
| transcript | `~/.claude/projects/<slug>/<uuid>.jsonl`，slug = cwd 把 `/` 换成 `-` |
| 子 agent | `<slug>/<uuid>/subagents/**/agent-<agentId>.jsonl`，**带的是父 sessionId** |
| 活跃注册表 | `~/.claude/sessions/<PID>.json`，**进程退出即删** |
| 体量 | 90 文件 / 212MB，最大 63MB；全量重解析实测 0.4s |

**结束检测（置信度最高）**：注册表文件消失 = 已结束。文件在但 PID 已死 = 崩溃。

⚠️ `procStart` 是 **UTC** 渲染的，而 `ps -o lstart` 打印**本地时间**。本机 CST+0800，直接字符串比较必然失败。要用 `TZ=UTC ps -p <pid> -o lstart=` 或都转成 epoch 再比。

⚠️ **token 重复陷阱**：assistant 记录按 content block 拆成多条，每条都带一份**完全相同**的 `message.usage`。实测某文件 26 条 assistant 记录只对应 7 个不同的 `requestId`。不按 `requestId` 去重会多算约 **3.7 倍**。

⚠️ 有 9 种记录类型**根本没有 `.timestamp` 字段**（last-prompt / mode / ai-title / permission-mode / file-history-snapshot / started / result / custom-title / agent-name），95 个文件里有 14 个以这类记录结尾。读最后一行的 timestamp 会拿到 null，必须往回扫。

**成本**：磁盘上没有。`stats-cache.json` 的 `costUSD` 对全部 7 个模型都是 `0`（Max 订阅），且该文件本身严重滞后（`lastComputedDate` 8-16，但 `dailyActivity` 停在 7-20）。只能自己按价格表算。

---

## Codex

⚠️ **绝不全量扫描**：`sessions/` 368 文件 675MB（最大 108MB），`archived_sessions/` 1043 文件 **12.97GB**（最大 268MB）。

**索引优先**：`~/.codex/state_5.sqlite` 的 `threads` 表（71,974 行），`tokens_used` 已预聚合，实测与 rollout 里最后一个累计值**完全相等**。

⚠️ 表里 **70,932 行指向已不存在的 rollout 文件**（实际只剩 368 个）。打开前必须 `exists()`。

⚠️ **最关键的一条修正**：线程主键是 `session_meta.payload.id`（= 文件名 UUID = `threads.id`），**不是** `payload.session_id` —— 后者是子 agent 的**根/父**线程 id。实测 8 月 117 个 rollout 里有 **92 个**两者不同；用错会把所有子 agent 折叠进父线程。

**token 正确算法**：`event_msg/token_count` 的 `.payload.info.total_token_usage` 是**累计值**且实测严格单调不减。取相邻累计值的**差**，dedup key 用 `<thread>#<累计总数>`。
- 直接累加 `last_token_usage` → 多算 4.4%（该记录在额度刷新时也会触发）
- 直接累加 `total_token_usage` → 三角形垃圾（实测 842,789,460 vs 真实 16,247,192）

⚠️ **写锁不可靠**：`~/.codex/thread-writer-locks/<id>.lock` 实测正在写入的 `codex exec` 线程**根本没有锁**，而一个锁被持有了 5.5 小时。用 `threads.updated_at_ms` + rollout mtime 判断，锁只当弱提示。

**额度**：`token_count` 记录同时带 `.payload.rate_limits.{primary:{used_percent,window_minutes,resets_at},credits,plan_type}` —— 这是 Codex 唯一的实时额度来源，且只在有 turn 跑的时候才更新。

⚠️ Codex 的本地日志库（`logs_*.sqlite`，可到 GB 级）与全局状态文件中实测出现过明文会话凭据（`set-cookie` JWT、裸 JWT）。这两个文件永不外泄，也不要读。

---

## DSH

`~/.dsh/sessions/<slug>/<id>/session.jsonl.zstd`，zstd 压缩。极小：6 个会话共 216KB。

- 第 1 行是 `session` 记录且**没有 `seq`**：`{type,version,id,createdAt,cwd,delegationDepth,agentPreset}`，子 agent 多带 `parentSession` / `origin:"subagent"`
- session id：顶层是 `session-<uuid>`，子 agent 是裸 `<uuid>`
- token 在 `assistant/message.data.usage.{inputTokens,outputTokens,cacheReadTokens,reasoningTokens}`，**每消息一条，不累计不重复**
- ⚠️ deepseek 的 `outputTokens` **已包含** `reasoningTokens`（实测 out 1313 / reasoning 418），不要重复计价
- 忽略全部 `*-chunks` 和 `assistant/chunk` —— 那是流式轨迹，已被 `assistant/message` / `tool/call` 完全取代
- **结束信号**：顶层会话最后一条是 `session/end-seed` = 干净结束。文件中段出现的 end-seed 表示结束后又被恢复，只认最后一条。子 agent 不发这个，它们停在 `turn/end`
- ⚠️ `seq` 有跳号是**正常的**（chunk 批处理把多个 seq 压成一行），不是损坏

---

## 用量与额度

| 来源 | 路径 | 新鲜度 |
|---|---|---|
| Claude 5h/7d 百分比 | `~/.claude/plugins/claude-hud/.usage-cache.json` | 5 分钟 TTL |
| Claude 详细额度 | `~/.claude.json` → `$.cachedUsageUtilization` | 只由 Claude Code 自己刷新 |
| Codex 周额度 | 最新 rollout 最后一条 `token_count` 的 `.payload.rate_limits` | 实时，但无 turn 时冻结 |
| 价格表 | `~/.hermes/models_dev_cache.json`（3.6MB models.dev 快照） | 需从 models.dev/api.json 刷新 |
| DeepSeek 余额 | **本地没有**，只能调 `GET api.deepseek.com/user/balance` | — |

⚠️ claude-hud 缓存的字段嵌在 **`$.data`** 下（并镜像在 `$.lastGoodData`），不在顶层。读 `$.fiveHour` 只会拿到 undefined。

⚠️ `cachedUsageUtilization` 里的 `seven_day_opus` / `seven_day_sonnet` / `seven_day_cowork` 本机**全是 JSON null**，只有 `five_hour` / `seven_day` / `nimbus_quill` 有值。

⚠️ `~/.codex/auth.json` 的 `OPENAI_API_KEY` 值是 JSON `null`（不是空串），做真值判断时注意。

**三家都不落盘美元成本**，任何金额都是我们用价格表估算的，UI 必须标明是估算。订阅制下真正的预算是百分比，不是钱。

---

## 安全

⚠️ **transcript 里会有活的凭据**。Codex 的 `compacted` 记录会重放历史用户输入 —— 用户当初粘进对话的 API key 就这样原样留在了本地记录里，而且会随压缩反复出现。这不是假设，是本项目开发过程中实测遇到的情况。

这就是 `redact.rs` 存在的唯一理由：任何送往远端的文本（摘要）必须先过它。

凭据文件（只读键名，永不读值）：`~/.claude/.credentials.json`、`~/.claude/sessions/<PID>.<sha256>.key`、`~/.codex/auth.json`、`~/.dsh/.credentials.yaml`、`~/.dsh/brain-http.env`、`~/.hermes/.env`，以及 macOS 钥匙串条目 `Claude Code-credentials`。

⚠️ Codex 的 `send_message` 跨 agent 载荷是 **Fernet 加密**的（`arguments` 以 `gAAAAA` 开头），从 rollout 读不到内容，拓扑只能靠 `thread_spawn_edges`。

---

## 显示器几何

| | Mi Monitor（主） | HP 25x（竖屏） |
|---|---|---|
| 逻辑分辨率 | 1920×1080 @2x | **1080×1920 @1x** |
| `NSScreen.frame` | (0, 0, 1920, 1080) | **(1920, −840, 1080, 1920)** |
| `visibleFrame` | (53, 0, 1867, 1050) | 与 frame **相同** |

竖屏**不保留任何系统区域**（无 Dock、无菜单栏），所以底部条 frame = `x=1920, y=-840, w=1080, h=140`。

⚠️ 不要硬编码 display id（重新插拔会变），用 `NSScreen.screens.first { $0.frame.height > $0.frame.width }` 挑竖屏。

⚠️ 本机实测的 window level 原始值：`normal=0 floating=3 mainMenu=24 statusBar=25 screenSaver=1000` —— `.statusBar` 在菜单栏**之上**，不是之下。

**macOS 没有任何公开 API 能像 Dock 那样预留屏幕空间**（只有私有 SkyLight 能做）。替代方案是 `.fullScreenAuxiliary` 浮在全屏应用之上。

---

## 与 dsh-brain 的关系

`dsh-brain` 插件里已有 work-tracking：hook 驱动（`UserPromptSubmit` / `Stop`）、每轮只存 goal + final answer、默认关闭、保留 7 天。那是「我今天干了啥」的沉淀，本项目是实时监控，两者数据模型不同，不合并。

但照抄它三样东西：hook 安装器**合并而非覆盖** `settings.json` 的做法、事件日志的 `version` + `idempotencyKey` + `producer` 设计、以及写锁格式 `{schemaVersion,pid,startedAt,token}`（pid + 随机 token 能识别 PID 复用后的陈旧锁）。
