# 和 hook 结合

`auto-memory-rs` 不加载插件、不跑后台 hook 脚本；它提供 **被 hook 调用的一方**：一个
内建的 `auto-memory hook` 前端（见 §8）和随附的 `plugins/agents` 插件包。可以结合的有四层，
按"可靠性 / 成本"排序：

| 层 | 谁触发 | 用来做什么 | 代价 |
|---|---|---|---|
| 文件系统 | `watch` 守护进程（notify + 1000 ms 去抖） | Obsidian/编辑器一改文件就更新索引 | 已经内建，无需配置 |
| git | `post-commit` / `post-merge` / `post-checkout` | "提交即索引"，兜住 watch 没跑的场景 | 一行 shell，本地即用 |
| agent 会话 | 内建 `auto-memory hook` + `plugins/agents`（Codex）/ Tact 插件的 command hook | 会话开始喂上下文（briefing）、压缩后 checkpoint | 插件目录；引擎已内建。**Tact 的 `SessionStart` 会被丢弃，喂 briefing 得挂 `UserPromptSubmit`（§1）** |
| agent 进程内 | Tact 的 `Hook` trait（Rust） | 同上，但同进程直调，无子进程 | 要改 Tact 代码；`SessionStart` 同样注入不了（§6） |

三层可以并存：它们最终都只是"读 vault、写同一个 SQLite 索引"。索引的写由 SQLite
（WAL + `busy_timeout=10s`）串行化，读完全无冲突。

---

## 1. 命令 hook 的契约（stdin/stdout 两边一致，采纳范围不同）

两套 harness 都沿用 Claude Code 的插件 hook 契约：

```
stdin : 一个 JSON 对象 —— session_id, transcript_path, cwd, hook_event_name, …
stdout: 一个 JSON 对象 —— {"hookSpecificOutput": {"hookEventName": …, "additionalContext": …}}
        （也接受较新的 {"decision": …, "additionalContext": …}）
        SessionStart / UserPromptSubmit 还接受**纯文本** stdout，直接当作 context
exit  : 永远 0 —— hook 失败绝不能弄坏会话
```

实现细节（两边源码）：

- Tact：`crates/tact/src/plugin/hooks.rs` —— 13 个事件，非零退出/超时/JSON 非法都只
  记 warning 并继续；命令字符串里的 `${CLAUDE_PLUGIN_ROOT}` / `$CLAUDE_PLUGIN_ROOT`
  会被展开，环境变量里也会带上 `CLAUDE_PLUGIN_ROOT`。
- Codex：插件 `hooks/hooks.json`，每个命令拿到同样的 stdin JSON；参考实现的
  `session_start.py` 明确写着 fail-open（`except BaseException: pass; sys.exit(0)`）。

但"输出被接受"不等于"输出被采纳"：Tact 会解析 `additionalContext`，却只在部分事件上把它接进
会话——`SessionStart` 恰恰是**不接**的那个（对应分支里只有一条 `warn!`：
`plugin SessionStart hook returned additional context; not applied in v1`）。所以 Codex 上
好用的"启动喂 briefing"，原样搬到 Tact 上等于没有。

| 事件 | Tact 对 `additionalContext` 的处理 |
|---|---|
| `SessionStart` | ❌ 丢弃，只 `warn!`（`system_prompt` 同样丢弃） |
| `UserPromptSubmit` | ✅ 追加到本次 prompt 文本（`prompt.push_str(extra)`） |
| `PreToolUse` | ✅ 塞进 tool input 的 `_hook_context` 字段 |
| `SubagentStart` | ✅ 追加到子 agent 的 system prompt |
| `PostToolUse` | 只支持 `suppress_output`（清空工具结果），不接 `additionalContext` |

（都在 `crates/tact/src/plugin/hooks.rs` 的对应分支。）

**因此：脚本只要能读 stdin JSON、把 brief 写 stdout、永远 exit 0，两边都能被加载；但要真把
brief 喂进上下文，Codex 用 `SessionStart`，Tact 得挂 `UserPromptSubmit`。**

---

## 2. `tools/auto-memory-hook.py`

仓库里带了一个这样的脚本（无第三方依赖，只用标准库）：

| 事件 | 行为 |
|---|---|
| `SessionStart` | 列最近 `AUTO_MEMORY_DAYS`（默认 7）天改动的笔记 |
| `UserPromptSubmit` | 用 prompt 文本检索，列相关笔记 |

配置全走环境变量（同一个脚本可服务多个 vault）：

```bash
AUTO_MEMORY_INDEX=/home/me/.local/share/auto-memory/memory.db   # 默认值同左
AUTO_MEMORY_PROJECT=oracle                                    # 必填：项目 permalink
AUTO_MEMORY_BIN=/home/me/.cargo/bin/auto-memory                 # 默认 auto-memory（PATH）
AUTO_MEMORY_DAYS=7        # SessionStart 回溯窗口
AUTO_MEMORY_LIMIT=8       # 每次 brief 的结果数
AUTO_MEMORY_QUERY=…       # 给了就用它代替"最近改动"检索
# 按目录覆盖（cwd 转 slug，如 /home/me/vault → HOME_ME_VAULT）：
AUTO_MEMORY_PROJECT_HOME_ME_VAULT=oracle
```

实测（对着 §integration-guide 里的 demo 索引）：

```console
$ echo '{"hook_event_name":"SessionStart","cwd":"/tmp/am-demo/vault"}' | python3 tools/auto-memory-hook.py
{"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": "Notes changed in the last 7 days:\n- Analytical Engine (demo/organizations/analytical-engine)\n- Ada Lovelace (demo/people/ada-lovelace)"}}

$ echo '{"hook_event_name":"UserPromptSubmit","prompt":"who was the first programmer"}' | python3 tools/auto-memory-hook.py
{"hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "additionalContext": "Related notes in the memory index:\n- Ada Lovelace (demo/people/ada-lovelace)"}}
```

失败路径也验过：索引不存在、stdin 为空、没有 `hook_event_name` → **静默 exit 0**，
宿主会话不受影响。

脚本会把 `auto-memory` 的日志留在 stderr（见 integration-guide §6.1），stdout 只有那一个
JSON 对象——这正是 hook 契约要求的纯净性。

---

## 3. 接线：git hook（实测可用）

vault 是 git 仓库时，把"提交"也变成一个触发点：

```bash
# .githooks/post-commit（chmod +x）
#!/bin/sh
auto-memory watch --vault "$(git rev-parse --show-toplevel)" \
  --index "${AUTO_MEMORY_INDEX:-$HOME/.local/share/auto-memory/memory.db}" \
  --project "${AUTO_MEMORY_PROJECT:-$(basename "$(git rev-parse --show-toplevel)")}" \
  --once >/dev/null

git config core.hooksPath .githooks     # 让 git 用仓库内的 hook，可随仓库分发
```

实测（临时仓库，两次提交）：

```
提交 #1 → hook 日志：INFO initial reconcile finished added=1 …    索引：entities=1
提交 #2 → hook 日志：INFO initial reconcile finished added=0 updated=1 …  索引：observations=2
```

要点：用 `--once`（处理一批就退出），别在 hook 里跑 `--full`（全量重建，慢且没必要）；
hook 里的 `--vault/--index/--project` 必须写全，别依赖 cwd。
`post-merge` / `post-checkout` 同理（别人 push 过来、切分支后补索引）。

---

## 4. 接线：Tact 插件 hook

Tact 的 marketplace 有两个来源：`tact-ui plugin marketplace add <Git URL 或 owner/repo 短写>`，
以及**自动发现的本地目录**。`file://` **不被接受**（`MarketplaceSource::parse` 只认 git / http /
https / ssh 和 `owner/repo`），本地目录只能走自动发现：

- `$HOME/.agents/plugins/marketplace.json`（个人）
- 从 cwd 逐级向上找到的第一个 `<root>/.agents/plugins/marketplace.json`（仓库内）

本仓库已经带了后者，cwd 在仓库里就能直接装：

```bash
cd /path/to/auto-memory-rs
tact-ui plugin marketplace list          # 应看到 auto-memory（discovered）
tact-ui plugin install auto-memory-rs@auto-memory
tact-ui plugin list                      # 确认已安装
```

```
auto-memory-rs/
├── .agents/plugins/marketplace.json    # Tact 从这里发现 marketplace
└── plugins/agents/                     # 插件本体
    ├── .codex-plugin/plugin.json       # 插件清单（name 必须等于 catalog 里的 name）
    └── hooks/hooks.json                # 事件 → 命令
```

```json
// .agents/plugins/marketplace.json —— source 相对 marketplace 根解析，且不能越出根目录
{ "name": "auto-memory", "plugins": [ { "name": "auto-memory-rs", "source": "./plugins/agents" } ] }
```

插件至少要贡献 skills / commands / hooks / MCP 之一才会被接受。`hooks/hooks.json` 的写法
（`$CLAUDE_PLUGIN_ROOT` 由 Tact 展开）：
```json
// hooks/hooks.json
{
  "hooks": {
    "SessionStart": [ { "matcher": "startup|resume|compact",
      "hooks": [ { "type": "command",
                   "command": "python3 \"$CLAUDE_PLUGIN_ROOT/hooks/auto-memory-hook.py\"",
                   "timeout": 10, "statusMessage": "Briefing from Auto Memory" } ] } ],
    "UserPromptSubmit": [ { "matcher": "",
      "hooks": [ { "type": "command",
                   "command": "python3 \"$CLAUDE_PLUGIN_ROOT/hooks/auto-memory-hook.py\"",
                   "timeout": 10, "statusMessage": "Searching memory" } ] } ]
  }
}
```

⚠️ 上面那份 `hooks.json` 里的 **`SessionStart` 在 Tact 上不会生效**：脚本会被执行，返回的
brief 却会被 `warn!` 丢掉（见 §1 的表）。要在 Tact 上真的喂进上下文，把这个脚本挂到
`UserPromptSubmit`（Tact 会把它追加进本次 prompt），`SessionStart` 那段留给 Codex。

注意：Tact 只展开 `${CLAUDE_PLUGIN_ROOT}`（不是 `${PLUGIN_ROOT}`），同时也会把它放进
环境变量——上面用 `"$CLAUDE_PLUGIN_ROOT/…"` 交给 shell 展开，两种 harness 都成立。
脚本里的环境变量（`AUTO_MEMORY_*`）要在**宿主进程**里导出（hook 继承宿主环境）。

---

## 5. 接线：Codex 插件 hook

与上面同构，`hooks/hooks.json` 的字段一样（Codex 的插件清单是
`.codex-plugin/plugin.json`，marketplace 用 `[marketplaces.<name>] source_type = "git"`）。
区别只在"谁来装"：Codex 的插件从 marketplace 装，编辑已装的官方插件会在更新时被覆盖
——要改就跑自己的插件仓库（内容同上）。

你现在的 `~/.codex/config.toml` 里那条 `[hooks.state."codex@basic-memory:hooks/hooks.json:…"]`
就是 Codex 记录的、来自旧插件的 hook 信任哈希；换成自己的插件后，
会多出对应你仓库的一条。

---

## 6. 接线：Tact 进程内 hook（最快、也最"深"）

如果不想走插件与子进程，Tact 的 hook 本身是 Rust trait（`crates/tact/src/hook/mod.rs`），
`tact-ui` 已经注册了一个（目前是空实现，见 `crates/tact-ui/src/interactive.rs`）：

```rust
// tact-ui 侧（现状）
agent.with_session_start(|_agent| Box::pin(async move { Ok(HookControl::Continue) }))
```

注意签名是 `Fn(&LoopState) -> Result<HookControl>`，而 `LoopState = Agent`
（`crates/tact/src/lib.rs`）——**入参只有不可变引用，返回值也只有 Continue/Block**，
所以进程内 `SessionStart` 和插件 `SessionStart` 一样注入不了 briefing。

要注入就用 `with_user_prompt_submit`（拿到 `&mut String`，会追加进 prompt），在里面
同进程直查 auto-memory：

```rust
// tact-ui 侧（示意）
// Cargo.toml: auto-memory-rs = { path = "../auto-memory-rs" }   // lib 名是 auto_memory
agent.with_user_prompt_submit(|_agent, prompt: &mut String| Box::pin(async move {
    // auto_memory::storage::Store + auto_memory::application::activity
    if let Ok(brief) = brief_from_index().await {
        prompt.push_str(&brief);
    }
    Ok(HookControl::Continue)
}))
```

好处：没有子进程、没有 JSON 往返、不会超时；代价是要改 Tact 的代码并且直接依赖
`auto-memory` crate（两个项目都是 Rust，可行）。适合"我就是想把记忆接进 tact-ui"的场景。

---

## 7. 设计约束与坑

- **Tact 的 `SessionStart` 注入不了**：脚本会被执行，但 `additionalContext` 只进 warning
  日志（§1）。要在 Tact 上喂上下文就挂 `UserPromptSubmit`；`SessionStart` 留给 Codex。
- **只能在 stdout 写约定内容**：`auto-memory` 自己把所有日志写 stderr（integration-guide
  §6.1），hook 脚本也别往 stdout 打调试信息——那会被当成 context 注入。
- **超时要短**：Tact/Codex 的 `timeout` 建议 10 s 内；脚本内部对 `auto-memory` 的调用
  也设了 8 s 上限。
- **fail-open**：所有失败路径 exit 0。会话开始时索引还没建好，也不该让 agent 起不来。
- **别在 hook 里 `reindex --full`**：那是全量扫描，会拖慢会话启动；用 `watch --once`
  或 `search`。
- **`--vault/--index/--project` 显式写**：hook 的 cwd 未必是你以为的那个（Codex 的
  hook payload 里有 `cwd`，脚本已经用它做按目录映射）。
- **路径写错的代价**：`--vault` 指错目录时 reconcile 会 prune 掉该项目的索引行
  （architecture-guide §3.0），hook 里尤其要小心。

---

## 8. 内建 hook 前端与 `plugins/agents`

`auto-memory` 现在自带一个 harness hook 前端，移植自参考实现的
`basic_memory.cli.commands.hook`：

```
auto-memory hook <session-start|pre-compact> --harness <claude|codex|pi> \
    [--index <db>] [--project <permalink>] [--project-dir <dir>]
```

契约与 §1 完全一致：stdin 一个 JSON 对象，stdout 只放 brief，**任何失败都 exit 0**
（fail-open）。实现分层：

| 文件 | 作用 |
|---|---|
| `src/hooks/profiles.rs` | 每 harness 的默认值/文案（recall 窗口、capture 目录、session note type） |
| `src/hooks/event.rs` | 把各 harness 的 stdin JSON 归一成 `NormalizedHookEvent` |
| `src/hooks/settings.rs` | 合并 user / project 配置（`.codex/basic-memory.json`、`.claude/settings.json` 的 `basicMemory` 块）；坏文件 **fail-closed** |
| `src/hooks/brief.rs` | 组装 SessionStart brief（fenced 数据 + 截断边界 + 写作指引） |
| `src/hooks/checkpoint.rs` | 压缩后的 checkpoint 提示（附 host 元数据） |

`plugins/agents/` 是配套的插件包：清单 + `hooks/hooks.json` + 两个 fail-open 薄壳
（`sh "${PLUGIN_ROOT}/hooks/session_start.sh"`）+ skills + schemas。**Codex 忽略
PreCompact 的 stdout**，所以 checkpoint 请求由压缩后那次 `SessionStart`
（`trigger: compact`）带出；`am-checkpoint` skill 再用 MCP `write_note` 写一条不可变的
`codex_session` / `coding_session` 笔记。详见 `plugins/agents/README.md`。

已实现的两个 verb 之外，参考实现还有 `hook stop|flush|status|install|remove`、SPEC-55
envelope/inbox WAL、transcript 抽取与自动 capture 笔记、以及 claude-code / pi 插件包，
**本切片尚未移植**。仓库里旧的 `tools/auto-memory-hook.py` 仍可用（§2），与内建前端并存。

---

## 9. 目前的能力缺口

脚本用 `search --after-date <window>` 近似"最近活动"，因为 **CLI 没有 `recent_activity`
的等价命令**（MCP 有 `recent_activity`，CLI 只有 `search` / `context` / `status`）。
要做真正的 briefing（按项目、按时间窗、带关系摘要），加一个
`auto-memory recent --index … --project … --timeframe 7d` 子命令即可——复用现成的
`application::activity`，改动很小。需要的话告诉我。
