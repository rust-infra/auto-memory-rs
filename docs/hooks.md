# 和 hook 结合

`auto-memory-rs` 不加载插件、不跑后台 hook 脚本；它提供 **被 hook 调用的一方**：一个
内建的 `auto-memory hook` 前端（见 §8）和随附的 `plugins/agents` 插件包。可以结合的有四层，
按"可靠性 / 成本"排序：

| 层 | 谁触发 | 用来做什么 | 代价 |
|---|---|---|---|
| 文件系统 | `watch` 守护进程（notify + 1000 ms 去抖） | Obsidian/编辑器一改文件就更新索引 | 已经内建，无需配置 |
| git | `post-commit` / `post-merge` / `post-checkout` | "提交即索引"，兜住 watch 没跑的场景 | 一行 shell，本地即用 |
| agent 会话 | 内建 `auto-memory hook` + `plugins/agents`（Codex）/ `plugins/tact` / `plugins/deepseek` 的 command hook | 会话开始喂上下文（briefing）、压缩后 checkpoint | 插件目录；引擎已内建。Tact 的 `SessionStart` 现在会采纳 `additionalContext`（§1），挂 `SessionStart` 即可。DSH 还没有命令 hook 宿主，见 §4b |
| agent 进程内 | Tact 的 `Hook` trait（Rust） | 同上，但同进程直调，无子进程 | 要改 Tact 代码；要共享句柄得自己捕获 `Arc<Mutex<Store>>`（§6） |

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
  记 warning 并继续；命令里的 `${CLAUDE_PLUGIN_ROOT}` / `$CLAUDE_PLUGIN_ROOT` 与
  `${PLUGIN_ROOT}` / `$PLUGIN_ROOT`（连同 `…_PLUGIN_DATA` 那一对）都会被展开，
  环境变量里也会带上。
- Codex：插件 `hooks/hooks.json`，每个命令拿到同样的 stdin JSON；参考实现的
  `session_start.py` 明确写着 fail-open（`except BaseException: pass; sys.exit(0)`）。

Tact 现在把 `additionalContext` 接进会话的事件比早期版本多，**包括 `SessionStart`**——早先
那条 `plugin SessionStart hook returned additional context; not applied in v1` 的 `warn!`
分支已经被 `collect_session_start_output` 取代（源码注释点名了原因：把 briefing 丢掉让参考
实现的 `basic-memory` 插件形同虚设）。所以"启动喂 brief"在 Tact 上是有效的。

| 事件 | Tact 对 `additionalContext` 的处理 |
|---|---|
| `SessionStart` | ✅ 收进 `SessionStartContext`，首轮之前注入为合成的 `<hook-context>` user 消息（`inject_pending_session_context`） |
| `UserPromptSubmit` | ✅ 追加到本次 prompt 文本（`prompt.push_str(extra)`） |
| `PreToolUse` | ✅ 排进 `runtime.pending_hook_context`，下一次请求前注入（**不再**写进 tool input 的 `_hook_context`；那个字段只写不读，已废弃） |
| `PostToolUse` | ✅ 与 `PreToolUse` 同一条注入路径；另外仍支持 `suppress_output`（清空工具结果） |
| `SubagentStart` | ✅ 追加到子 agent 的 system prompt |
| `SubagentStop` | ✅ 可改写子 agent 回给父 agent 的 summary |

（都在 `crates/tact/src/plugin/hooks.rs` 的对应分支。命令 hook 还能用
`additionalContextLimit`（token 数）声明"最多喂多少"，超出的部分在进入上下文前被截断。）

**因此：脚本只要能读 stdin JSON、把 brief 写 stdout、永远 exit 0，两边都能被加载；brief 也
在两边都能真的进上下文——Codex 和 Tact 都挂 `SessionStart` 即可。**

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

本仓库已经带了后者，cwd 在仓库里就能直接装（Tact 装 `plugins/tact`，Codex 装
`plugins/agents`——**每个 host 只装自己那一个包**，理由见 §8）：

```bash
cd /path/to/auto-memory-rs
tact-ui plugin marketplace list          # 应看到 auto-memory（discovered）
tact-ui plugin install auto-memory-tact@auto-memory
tact-ui plugin list                      # 确认已安装
```

```
auto-memory-rs/
├── .agents/plugins/marketplace.json    # Tact 从这里发现 marketplace
└── plugins/tact/                       # 插件本体（Tact 版）
    ├── .codex-plugin/plugin.json       # 插件清单（name 必须等于 catalog 里的 name）
    └── hooks/hooks.json                # 事件 → 命令
```

```json
// .agents/plugins/marketplace.json —— source 相对 marketplace 根解析，且不能越出根目录
{ "name": "auto-memory", "plugins": [
  { "name": "auto-memory-rs",   "source": "./plugins/agents" },
  { "name": "auto-memory-tact", "source": "./plugins/tact" } ] }
```

插件至少要贡献 skills / commands / hooks / MCP 之一才会被接受。`hooks/hooks.json` 的写法
（`$CLAUDE_PLUGIN_ROOT` 由 Tact 展开）——薄壳只做 `command -v` 兜底再调 `auto-memory hook`，
逻辑在引擎里，所以脚本本身与 harness 无关：
```json
// hooks/hooks.json
{
  "hooks": {
    "SessionStart": [ { "matcher": "startup|resume|compact",
      "hooks": [ { "type": "command",
                   "command": "sh \"$CLAUDE_PLUGIN_ROOT/hooks/session_start.sh\"",
                   "timeout": 30, "additionalContextLimit": 4000,
                   "statusMessage": "Loading Auto Memory context" } ] } ],
    "PreCompact": [ { "matcher": "manual|auto",
      "hooks": [ { "type": "command",
                   "command": "sh \"$CLAUDE_PLUGIN_ROOT/hooks/pre_compact.sh\"",
                   "timeout": 60,
                   "statusMessage": "Checkpointing Tact work to Auto Memory" } ] } ]
  }
}
```

这份 `hooks.json` 在 Tact 和 Codex 上都能用：`SessionStart` 的 brief 会被注入（见 §1 的表）；
`PreCompact` 那段只是占位——两边都忽略它的 stdout，checkpoint 请求由压缩后那次
`SessionStart(trigger=compact)` 带出（见 §8）。装完记得让 hook 通过信任检查——
`tact-ui hooks list` 会把它列在 "Needs review" 下，`tact-ui hooks trust`（或 `--all`）
之后才会真的执行。

注意：Tact 会把**四个**占位符都展开（`crates/tact/src/plugin/hooks.rs:886`
`expand_plugin_placeholders`），带括号和裸写两种形式都认：`CLAUDE_PLUGIN_ROOT` / `PLUGIN_ROOT`
（= 插件缓存根目录）、`CLAUDE_PLUGIN_DATA` / `PLUGIN_DATA`（= 可写目录，跨升级保留）。
"两个 ABI 的名字都接受"是**修过之后**的行为——源码注释点名了修之前的样子：
*"Accepting only the Claude root spelling used to leave the Codex one to `sh`, where an unset
`${PLUGIN_ROOT}` expands to the empty string and the hook silently addressed `/hooks/...`."*
同一批名字也会作为环境变量导出，所以 `"$CLAUDE_PLUGIN_ROOT/…"` 和 `"${PLUGIN_ROOT}/…"` 都成立。
脚本里的环境变量（`AUTO_MEMORY_*`）要在**宿主进程**里导出（hook 继承宿主环境）。

---

## 4b. 接线：DeepSeek Harness（DSH）

DSH 的插件模型和 Codex/Tact 不是一回事，接线前先认清这张表：

| 能力 | Codex / Tact | DSH |
|---|---|---|
| MCP server | `config.toml` / `.mcp.json` | profile 的 `cordis.patch.yml` 里一条 `@deepseek-ai/dsh-mcp-client` |
| 命令 hook（`SessionStart` / `PreCompact`） | `hooks/hooks.json` | **没有**——`dsh-mcp-client` 只桥接 MCP tools |
| Skills | 插件 `skills/` 目录 | 扫 `<root>/.dsh/skills`、`<root>/.agents/skills`、`$DSH_HOME/skills`、`~/.agents/skills` |

所以在 DSH 上真正生效的是 **skills + MCP server** 这两条；brief 靠 `am-orient` 按需拉，
不是会话启动时注入。`plugins/deepseek/hooks/` 仍然随包提供（契约与 Codex 同源、零成本，
将来 DSH 有了 hook 宿主可以直接用），但现在不会被执行。

**推荐：装成 DSH 插件（bundle）**——DSH 的插件就是 npm 包，`dsh plugin` 转发给 pnpm：

```bash
dsh plugin --profile desktop add /Users/rg/Projects/auto-memory-rs/dsh-plugin
```

它会往 profile 的 `dsh.profile.bundles` 里追加 `auto-memory-dsh`，并由包自带的
`cordis.patch.yml` 插入两条 row：一条把包内的 `skills/` 注册成 skill provider
（`providerName: auto-memory`、`includeDefaultRoots: false`），一条挂 `auto-memory mcp`。
skill 根目录用 `createRequire(baseUrl).resolve('auto-memory-dsh/package.json')` 解析——
pnpm 的虚拟 store 布局下只有这种方式是对的。详见
[`dsh-plugin/README.md`](../dsh-plugin/README.md)。

包内的 row **不带** `--project` / `--vault` / `--index`，`auto-memory mcp` 会自己从
`~/.config/auto-memory/config.json` 解析（`index` + `default_project`），所以装完只需一条配置：

```bash
auto-memory project default 1m     # 写 default_project；索引里没有这个 permalink 会拒绝
auto-memory project list           # 默认项前面有 `*`
```

还没注册过 vault 的话一步到位：`auto-memory project add "<名字>" <目录> --default`。
`index` 可选，默认 `~/.local/share/auto-memory/memory.db`。

`default_project` 填 **permalink**（`auto-memory project list` 的 `permalink` 列），不是展示名。
要改某个 profile 指向别的 vault/project，再按 id 覆盖：

```yaml
- id: auto-memory-mcp
  config:
    args: ["mcp", "--project", "other", "--vault", "/path/to/vault", "--read-only"]
```

**手工方式**（不装 npm 包时）——把 skills 链进用户级扫描根：

```bash
mkdir -p ~/.dsh/skills
ln -sfn "$PWD/plugins/deepseek/skills/"* ~/.dsh/skills/
```

**MCP server**——加进 `~/.dsh/profiles/<profile>/cordis.patch.yml`（桌面 App 用 `desktop`，
`dsh web` 用 `web`）：

```yaml
- id: mcp-auto-memory
  name: '@deepseek-ai/dsh-mcp-client'
  config:
    serverName: memory
    transport: stdio
    command: /Users/you/.cargo/bin/auto-memory
    args: ['mcp', '--project', '1m', '--read-only']
    toolCallTimeoutMs: 60000
```

存盘即热插拔，模型侧的工具名是 `mcp__memory__search_notes` / `mcp__memory__write_note` / …。
`--read-only` 会把 6 个写工具从 `tools/list` 里摘掉（20 → 14）。

三个坑：

- **`--project` 填 permalink，不是展示名**。先跑 `auto-memory project list`，取
  `permalink` 那一列；列表为空说明还没索引过 vault，先
  `auto-memory project add "<名字>" <vault目录>`。填错/没填的表现都是"客户端连上了但没有工具"。
  现在 `no project` 报错会直接列出这个索引里可用的 permalink，并提示用第一列。
- `failOnStartupError` 默认 `false`，连不上时**只是没有工具、不报错**。第一次调试建议临时设
  `true`。
- `mcp` 启动时会按 `--vault` 做一次 reconcile；vault 路径指错（存在但不对）会把该项目已有索引
  行 prune 掉。接完先看 `auto_memory_diagnostics` 里的 vault 行。

---

## 5. 接线：Codex 插件 hook

与上面同构，`hooks/hooks.json` 的字段一样（Codex 的插件清单是
`.codex-plugin/plugin.json`，marketplace 用 `[marketplaces.<name>] source_type = "git"`）。
区别只在"谁来装"：Codex 的插件从 marketplace 装，编辑已装的官方插件会在更新时被覆盖
——要改就跑自己的插件仓库（内容同上）。

Codex 会在 `~/.codex/config.toml` 里用一条 `[hooks.state."<plugin>:<hooks 文件>:<命令哈希>"]`
记录每个**已信任**的命令 hook。本机现在**没有**这一条——Codex 侧的 memory 插件当前并没装，
配置被重写过——所以别把"配置里有没有这条"当成 hook 是否生效的判据：装好插件后 Codex 才会写上
对应的一条，而一旦改了 hook 命令，哈希就变，需要重新信任。

---

## 6. 接线：Tact 进程内 hook（最快、也最"深"）

如果不想走插件与子进程，Tact 的 hook 本身是 Rust trait（`crates/tact/src/hook/mod.rs`），
注册点在 `crates/tact-ui/src/session_bootstrap.rs`（`apply_plugin_hooks_with_report` 那几行，
命令 hook 和 Rust hook 最终都挂到同一个 `Agent` 上）。

签名现在是**带可变上下文**的，`SessionStart` 注入得了 briefing：

```rust
// crates/tact/src/hook/mod.rs（现状）
pub trait SessionStartFn: for<'a> Fn(
    &'a LoopState,
    &'a mut SessionStartContext,   // push_additional_context(source, text)
) -> Pin<Box<dyn Future<Output = Result<HookControl>> + Send + 'a>> + Send + Sync {}
```

所以两条都能用——`SessionStart` 喂 briefing，`UserPromptSubmit` 做按 prompt 的召回
（它是唯一能拿到用户文本的那个，`&mut String`）：

```rust
// tact-ui 侧（示意）
// Cargo.toml: auto-memory-rs = { path = "../auto-memory-rs" }   // lib 名是 auto_memory
// Store 不是 Clone，服务全是 &mut Store 形状 → 用 Arc<tokio::sync::Mutex<Store>> 捕获进去
let store = Arc::new(tokio::sync::Mutex::new(Store::open(&index).await?));

let for_start = store.clone();
agent = agent.with_session_start(move |_agent, context: &mut SessionStartContext| {
    let store = for_start.clone();
    Box::pin(async move {
        let guard = store.lock().await;
        if let Ok(brief) = brief_from_index(&guard).await {   // 失败就 fail-open
            context.push_additional_context(Some("auto-memory"), &brief);
        }
        Ok(HookControl::Continue)
    })
});

let for_prompt = store.clone();
agent = agent.with_user_prompt_submit(move |_agent, prompt: &mut String| {
    let store = for_prompt.clone();
    Box::pin(async move {
        let guard = store.lock().await;
        if let Ok(extra) = recall_for(&guard, prompt).await {
            prompt.push_str(&extra);
        }
        Ok(HookControl::Continue)
    })
});
```

代价与注意：hook 闭包要求 `Fn + Send + Sync + 'static` 且只拿 `&LoopState`（**拿不到 agent
状态**，共享句柄只能自己捕获）；Rust hook 没有 `additionalContextLimit` 那个上限，得自己截断；
`Store::open` 走 `block_in_place`，在 `#[tokio::test]`（current-thread）里会 panic，
碰 memory 的测试要写 `flavor = "multi_thread"`。完整清单见 `docs/tact-ui-integration.md`。

好处：没有子进程、没有 JSON 往返、不会超时；代价是要改 Tact 的代码并且直接依赖
`auto-memory` crate（两个项目都是 Rust，可行）。适合"我就是想把记忆接进 tact-ui"的场景。

---

## 7. 设计约束与坑

- **Tact 的 `SessionStart` 现在注入得了**：`additionalContext` 会被收进 `SessionStartContext`，
  首轮之前注入为 `<hook-context>` 消息（§1）。挂 `SessionStart` 喂 briefing、挂
  `UserPromptSubmit` 做按 prompt 的召回（那是唯一能拿到用户文本的事件）。
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
auto-memory hook <session-start|pre-compact> --harness <claude|codex|pi|tact|deepseek> \
    [--index <db>] [--project <permalink>] [--project-dir <dir>]
```

契约与 §1 完全一致：stdin 一个 JSON 对象，stdout 只放 brief，**任何失败都 exit 0**
（fail-open）。实现分层：

| 文件 | 作用 |
|---|---|
| `src/hooks/profiles.rs` | 每 harness 的默认值/文案（recall 窗口、capture 目录、session note type、checkpoint 提示） |
| `src/hooks/event.rs` | 把各 harness 的 stdin JSON 归一成 `NormalizedHookEvent` |
| `src/hooks/settings.rs` | 合并 user / project 配置（`.codex/basic-memory.json` 的 `basicMemory` 块、`.tact/auto-memory.json` 与 `$DSH_HOME/auto-memory.json` 的 `autoMemory` 块、`.claude/settings.json`、`.pi/basic-memory.json`）；坏文件 **fail-closed** |
| `src/hooks/brief.rs` | 组装 SessionStart brief（fenced 数据 + 截断边界 + 写作指引） |
| `src/hooks/checkpoint.rs` | 压缩后的 checkpoint 提示（附 host 元数据；按 harness 选提示文本，元数据键沿用 skill 的 `codex_turn_id`） |

**`tact` 和 `deepseek` 是 port 自己加的 harness**（参考实现没有）：两者的 hook 契约都和
Codex 同源，stdin 字段也对得上（`session_id` / `cwd` / `source` / `turn_id` / `model`，只有
`transcript_path` 恒为 `null`），所以它们复用同一套引擎，差异只在三处——事件上盖的 identity
（`tact` / `deepseek`）、读的配置文件（`.tact/auto-memory.json` 与
`$DSH_HOME/auto-memory.json`，都是 `autoMemory` 块——**auto-memory 自己的文件**：参考实现
从不读 `.tact/` 或 `.dsh/` 下的任何东西，所以名字不用跟它一致）、
以及打印给用户的文案。

`deepseek` 与 `tact` 刻意不共用配置：两者的目录（`.tact/` vs `.dsh/`）、source id、以及
recall 的 note type 都不同。Tact 会额外 recall `codex_session`（共享 vault 里已有 Codex
笔记），DeepSeek **只** recall 自己写的 `deepseek_session`——"这条上下文是哪个 host 产出的"
必须能从 brief 本身回答。这条隔离由测试双向钉住：
`deepseek_ignores_the_codex_and_tact_mapping_files` /
`codex_and_tact_ignore_the_deepseek_mapping_file`，以及 `tests/hook.rs` 的
`cli_deepseek_does_not_inherit_a_codex_mapping`。

验证不需要起 TUI：

```bash
echo '{"hook_event_name":"SessionStart","cwd":"/abs/repo","source":"startup"}' \
    | auto-memory hook session-start --harness tact --index "$INDEX" --project oracle

echo '{"hook_event_name":"SessionStart","cwd":"/abs/repo","source":"startup"}' \
    | auto-memory hook session-start --harness deepseek --index "$INDEX" --project oracle
```

`plugins/` 下是配套的插件包，**每个 host 一个**：`plugins/agents`（Codex）、
`plugins/tact`（Tact）、`plugins/deepseek`（DeepSeek Harness）。三者是同一套
skills/schemas 的三个版本，差异只在 harness identity（薄壳的 `--harness`、skill 读的配置路径、
note type、标题前缀），详见 [tact-ui-integration.md](tact-ui-integration.md) §2.2 和各自的
README。**三个 host 都忽略 PreCompact 的 stdout**，所以 checkpoint 请求由压缩后那次
`SessionStart`（`trigger: compact`）带出；`am-checkpoint` skill 再用 MCP `write_note` 写一条
不可变的 `codex_session` / `tact_session` / `deepseek_session` / `coding_session` 笔记。

⚠️ **每个 host 只装自己那一个包**：Tact 和 DSH 都会加载所有可见根下的 skills，装两个就会
一次会话看到两套 `am-*`（`auto-memory-rs:am-checkpoint` 和
`auto-memory-tact:am-checkpoint`），模型可能挑错。

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
