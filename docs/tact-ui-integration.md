# 集成到 tact-ui：需要做哪些调整

**定位：`auto-memory` 和 Basic Memory 一样是独立产品，不是要链进 tact 的库。**
所以集成面只有两条进程边界——**MCP 子进程**和**插件 command hook**——要做的调整是"把
产品侧对 Tact 的适配补齐"，不是"改 tact 的代码把库塞进去"。

> 相关文档：[hooks.md](hooks.md)（hook 契约与四层触发点）、
> [integration-guide.md](integration-guide.md) §2（vault / index / project）。

---

## 1. 现状盘点：Tact 已经能吃到什么，缺什么

Tact 的 hook 契约（`crates/tact/src/plugin/hooks.rs`）和 Codex 同源，`auto-memory` 的内建
hook 前端 + `plugins/agents/` 包**大部分能直接跑**，缺的是"Tact 这个名字"。

| 项 | Tact 的现状 | auto-memory 的现状 | 要做的调整 |
|---|---|---|---|
| hook 事件 | 13 个，含 `SessionStart` / `UserPromptSubmit` / `PreCompact` / `PostCompact` | 只实现 `session-start` / `pre-compact` 两个 verb | 够用，不用扩 |
| `SessionStart` 注入 | ✅ 收 `additionalContext`，首轮前注入 `<hook-context>` | brief 走 stdout | 无需改 |
| 纯文本 stdout | ✅ SessionStart / UserPromptSubmit 的非 JSON stdout 直接当 context | 就是纯文本 | 无需改 |
| stdin payload 字段 | `session_id`（真实会话 id）、`cwd`、`hook_event_name`、`source`、`model`、`permission_mode`、`turn_id`、`transcript_path: null` | Codex 分支读 `source`→trigger、`turn_id`、`model` | 字段已对齐，但见 §2.1 |
| `--harness` | — | ✅ 已支持 `claude` / `codex` / `pi` / `tact` | 完成（§2.1） |
| 项目映射配置 | — | ✅ 另读 `~/.tact/basic-memory.json` + `.tact/basic-memory.json` | 完成（§2.1） |
| 插件包 | 读 `.codex-plugin/plugin.json` + `hooks/hooks.json`，`${CLAUDE_PLUGIN_ROOT}` 会展开 | 整包是 Codex 的（wrapper 硬编码 `--harness codex`） | 出 Tact 版措辞/wrapper |
| `PreCompact` 的 stdout | **忽略**（只用 `control` 做 veto） | 自己也早退，不打印 | 无需改，见 §2.4 |
| 市场来源 | `file://` 与裸本地路径都不接受；只认 `owner/repo` / git URL / 本地发现 | 仓库根有 `.agents/plugins/marketplace.json` | ✅ 已核对（§2.5）：**插件必须先 commit**，tact 从 HEAD 的 git tree 复制 |

---

## 2. auto-memory 侧要做的调整

### 2.1 加一个 `tact` harness（必须）— ✅ 已实现

四处，都是同构扩展：

- `src/hooks/profiles.rs` — `Harness::Tact` + `parse("tact")` + `profile()` 分支 + `const TACT`
  （`source: "tact"`、capture 目录按仓库分成 `tact/<repo>`）；另外加了 `TACT_CHECKPOINT_PROMPT`
  （提示文本和结尾措辞按 harness 选，Codex 那份逐字不变）。元数据键和 note type **刻意沿用
  skill 的词汇**（`codex_turn_id`、`codex_session`），理由见 §2.2 末段。
- `src/hooks/event.rs` — normalize 加 `Tact` 分支，与 Codex 共用 `turn_id` / `model`。
  `transcript_path: null` 被现有的 `field()` 归一成空，不用特判（有单测钉住真实 payload）。
- `src/hooks/settings.rs` — 新增 `load_tact_settings`：读 user `~/.tact/basic-memory.json` +
  最近的含 `<root>/.tact/basic-memory.json` 的祖先目录，坏文件 fail-closed。Codex 和 Tact
  的加载器合并成一个 `load_agent_dir_settings`（只有 `.codex` / `.tact` 和 profile 不同），
  capture 目录的按仓库分目录逻辑也合并成 `repo_scoped_capture_folder`。
- `src/main.rs` — `--harness tact` 可用；checkpoint 闸门从 Codex-only 放宽到
  `Codex | Tact`；**未知 `--harness` 改成 fail-open**（warn + exit 0，不再走 `usage()` 的
  exit 2），这样"任何失败都 exit 0"对整条命令成立。

验证（不用起 TUI，`tests/hook.rs` 里有对应的 CLI 测试）：

```bash
echo '{"hook_event_name":"SessionStart","cwd":"/abs/repo","source":"startup"}' \
    | auto-memory hook session-start --harness tact --index "$INDEX" --project oracle
```

### 2.2 插件包出 Tact 版 — ✅ 已实现

`plugins/agents/` 现在是一个 **Codex 包**。逐文件看耦合在哪：

| 文件 | 与 harness 的耦合 | Tact 版要改什么 |
|---|---|---|
| `.codex-plugin/plugin.json` | 目录名 `.codex-plugin/`（**tact 读的就是这个名，不用改**）；`name: auto-memory-rs`（必须等于 catalog 里的 id）；`interface.displayName` 是 "Auto Memory for Codex"；description 写 "Codex-native"；keywords 含 `codex` | 文案。另立一个包的话 `name` 必须换 |
| `hooks/hooks.json` | 事件名、正则 matcher、`timeout`、`additionalContextLimit` 都不含 harness；命令是 `sh "$CLAUDE_PLUGIN_ROOT/hooks/session_start.sh"`（两个 host 都展开这个变量） | **不用改** |
| `hooks/session_start.sh`、`hooks/pre_compact.sh` | 各 12 行：`command -v` 兜底 + 硬编码 `--harness codex` + `\|\| exit 0` | 这一行 |
| `skills/am-*.md`（6 个，约 530 行） | **正文写死 `~/.codex/basic-memory.json` 和 `.codex/basic-memory.json`**；description 里也点名 Codex（"Codex handoff"、"from Codex"）；`am-checkpoint` 还写死 `type: codex_session`、`agent: codex`、`tags: ["codex", …]`、标题前缀 `Codex checkpoint - …` | 路径 → `.tact/…`；措辞 → Tact |
| `schemas/codex-session.md` | `entity: CodexSession`，与 `am-checkpoint` 写的 `type: codex_session` 配对 | 复用（推荐）或另加一份 |
| `README.md` | 整篇是 Codex 的 | 文案 |

**约束（都已核对源码）**

- `plugin.json` 的 `name` 必须等于 catalog 里的 plugin id（`crates/tact/src/plugin/install.rs:454-458`），且同一个 id 不能从两个 marketplace 各装一次（`:166-168`）→ 两个包必须用两个不同的 name。
- 插件至少要贡献 skills / commands / hooks / MCP 之一才会被接受（`:448-452`）。
- **符号链接共享行不通**：`copy_regular_files` 只拷目录和普通文件（`install.rs:378-395`），symlink 既不是 dir 也不是 file，会被**静默跳过**。所以"第二个包 symlink 回第一个包的 skills"是死路，要共享只能放在同一个包目录里。
- 安装是**拷贝**（本地目录源也走 `copy_regular_files`），不是 Codex 那种 symlink 就地引用；`plugin update` 会重新拷一遍。

**两个选项**

- **(a) 一个包 + 一个环境变量**：薄壳改成 `harness="${AUTO_MEMORY_HARNESS:-codex}"`；清单和 skill 文案改中性；skill 里的路径写成"`~/.codex/basic-memory.json` 或 `~/.tact/basic-memory.json`"。
  好处只有一份 skill/schema；代价是 Tact 用户得在 shell rc 里 export 一个变量（hook 继承宿主环境）——**而这正是 §2.3 要消灭的配置方式**。
- **(b) 两个包**：`plugins/agents/`（Codex，不动）+ `plugins/tact/`（复制一份，改薄壳、清单 `name`、skill 里的路径与措辞、schema 的 entity 名）。
  好处是各自措辞正确、可独立更新，也和参考实现按 harness 分包的做法一致（本 port 的 README 说 skills/schemas 是从参考实现的 `plugins/codex` 移植的，claude-code / pi 那两份还没移植）；代价是约 600 行双份维护，catalog 要两条 entry。

**决定：(b)**。理由不是"重复维护麻烦"，而是 (a) 的身份来源本身不成立：

- tact 的 `HookCommand` **没有 `env` 字段**（`hooks.rs:164-200`），插件无法给自己的 hook 设
  环境变量 → (a) 的开关只能由用户在 shell rc 里全局 export，而 hook 继承的是**宿主进程**环境
  → 同一个包被两个 host 读时，设给一个就把另一个一起改掉，且没有 per-host 的补救办法。
- 更根本：harness 是"**哪个 agent 在跑这个 hook**"的属性，不是 shell 的属性。
- (a) 也不消除分歧，只是把分歧挪进 skill 正文（"读 `~/.codex/…` 或 `~/.tact/…`"），模型在
  Tact 下可能读错那一份，**写错地方还不报错**。给 agent 的指令里，歧义就是 bug。

落地方式：从 `plugins/agents/` 派生 `plugins/tact/`（薄壳 `--harness tact`、skill 路径改
`.tact/`、`agent: tact`、标题前缀 `Tact checkpoint - `、新增 `schemas/tact-session.md`），
并把 `TACT.session_note_type` / `recall_session_types` 翻回 `tact_session`、
`CHECKPOINT_TURN_ID_KEY` 翻回 `turn_id`——**两边同向改，不能只动一边**。
派生时别盲跑 `sed s/codex/tact/g`：有一类 "codex" 是在**指代另一个 host**（README 里
"从参考实现的 `plugins/codex` 移植"、MCP 段的 "For Codex, add the same entry under
`~/.codex/config.toml`"、schema 文件名 `codex-session.md`），这些要留住。

**两个包共存时的行为**（tact 不按 host 过滤插件，都装就会都生效）：

- **hook 会各跑一遍**：`apply_plugin_hooks` 遍历所有已安装插件（`hooks.rs:1000-1024`），
  两个包的 `SessionStart` 都注册 → 一次会话收到两份 brief（一份 Codex 措辞、读 `.codex/…`）。
  → **每个 host 只装自己那一个包**，这是用户步骤，包本身管不了。
- **skill 会各加载一遍**：`SkillRegistry` 加载所有已安装插件的 skills，按 plugin id 命名空间
  （`skill/mod.rs:96-104`、`:253-255`）→ `auto-memory-rs:am-*` 和 `auto-memory-tact:am-*`
  同时可见。→ checkpoint 提示应该写**带命名空间的全名**，否则模型可能挑中 Codex 那份。
- 两个包**不能用同一个 id**：同一 catalog 内重名会被拒（`marketplace.rs:149`），跨 marketplace
  同 id 也会被拒（`install.rs:166-168`）。所以第二个包必须换名——这正是换名的意义。
- 名字纯属标识，没有保留字或长度限制：`validate_plugin_id` 只挡空值和带路径分隔符的值
  （`install.rs:315-327`）。叫 `auto-memory-tact` 还是 `auto-memory-rs-tact` 只影响观感。
  但**改名有代价**：plugin id 同时是 skill 命名空间和 hook 信任标签（`plugin {id}`，
  `hooks.rs:1014`），改了要重新 `hooks trust`、文档里写全名的地方也要跟着改——所以趁包还没
  存在时定下来。

**引擎侧已经为这一步铺好的**（§2.1 那轮改动）：事件 identity、设置文件路径、打印文案都按 harness 分开了。元数据键和 note type 则必须**跟着包一起翻**（见下）。

**落地结果**（✅ 已完成）：

- 新增 `plugins/tact/`：由 `plugins/agents/` 派生（脚本做机械替换，然后人工过一遍指代另一个
  host 的地方）。清单 `name: auto-memory-tact`、薄壳 `--harness tact`、skill 路径改 `.tact/`、
  `agent: tact`、标题前缀 `Tact checkpoint - `、`schemas/codex-session.md` →
  `schemas/tact-session.md`（`entity: TactSession`）。
- `.agents/plugins/marketplace.json` 加第二条 entry；两份 README 互相指路并写明"每个 host 只装
  一个"。
- 引擎同向翻转：`TACT.session_note_type` / `recall_session_types` → `tact_session`；
  turn-id 键拆回两个常量（Codex `codex_turn_id` / Tact `turn_id`）。
- `TACT_CHECKPOINT_PROMPT` 现在写**带命名空间的全名** `auto-memory-tact:am-checkpoint`，
  避免模型挑中同时可见的 Codex 那份。
- 保留 `codex_session_id` 作为**兼容字段**：同一个 vault 里可能有 Codex 包写的旧笔记，
  Tact 包读它们时仍然接受这个字段（`am-checkpoint` 的 legacy 分支 + 两个 schema 的
  `codex_session_id?` 都留着，并注明是"Codex 的字段"）。

### 2.3 配置发现（产品化最关键的一条）

现在 vault / index / project 全靠 `AUTO_MEMORY_*` 环境变量，而 hook 继承**宿主进程**的环境
——用户得先在 shell 里 export 才有 brief。这对"独立产品"是不合格的默认：被配置的东西是
**per-host** 的，而通道是 per-process 的，两者不匹配。

**已出设计规格：[`specs/config-discovery-spec.md`](../specs/config-discovery-spec.md)**（2026-10-06），
执行计划 [`plans/config-discovery-plan.md`](../plans/config-discovery-plan.md)。
**Slice A + B 已落地**：`src/config.rs` 真正接上了（`Config` 加 `index` / `default_project`，
`load_user_config` 返回 Absent/Loaded/Malformed 让调用方各自决定策略），
`resolve_index` / `resolve_project` 是唯一的两条链并带 `Origin`；hook、`doctor`、所有 CLI
命令都走它们（`--index` 现在全部可选）；`--vault` 在 `reindex` / `watch` / `mcp` 上也可选，
不传就用项目注册表里的路径；hook 遇到坏配置 warn 后继续，CLI 直接报错；`doctor` 新增
`config` 检查并打印每个值的来源。要点：

- 三条解析链（index / project / vault）各自定死优先级，**flag 永远最高**，所以两个插件包
  （传的是 flag）不需要改。
- 新增用户级配置文件 `~/.config/auto-memory/config.json`——顺带让 `src/config.rs` 那个
  "全仓无人读取"的 `Config` 结构体真正接上（参考默认值不再硬编码）。
- **vault 可以从索引里推**：`projects` 行本来就存了 `path`（`records.rs:8-19`），所以
  `reindex` / `watch` / `mcp` 在项目注册之后不必再传 `--vault`；而且这比接受用户输入**更安全**
  ——路径打错不会再让 reconcile prune 掉索引行（integration-guide §2.1 那个坑）。
- 首跑提示改成可执行：列出索引里已注册的 permalink，让用户知道该往
  `.tact/basic-memory.json` 里填什么；**不做"只有一个项目就猜它"**——猜错比没有更糟。
- `doctor` 打印每个值的**来源**（哪个文件 / 哪一步 / 默认），排障一条命令。

这一步做完，tact-ui 侧就是**零配置**：装插件、trust、完事。

### 2.4 `PreCompact` 不要指望 stdout

Tact 和 Codex 一样只用它的 `control`（`plugin/hooks.rs:2353-2354` 只取 `output.control`），
`additionalContext` 被丢掉；`auto-memory` 自己的 `run_hook` 也在 `CompactionImminent` 上早退
（`src/main.rs:700-704`）。所以 **checkpoint 请求只能挂在压缩后那次
`SessionStart(trigger=compact)`**——现有设计是对的，`hooks.json` 里 `PreCompact` 那段的作用
只是"占位 + 允许 veto"。这条对 Tact 同样成立，文档里要写清楚（现在 `hooks.md` §8 只提了 Codex）。

### 2.5 分发路径 — ✅ 已核对（含一次真实安装）

**能作为 marketplace 源的形状**（`MarketplaceSource::parse`，`tact/src/plugin/model.rs:55-95`）：
`owner/repo` 简写、带 host 的 `git|http|https|ssh` URL、`git@host:path`。
**`file://` 和裸本地路径都不接受**——前者被 scheme 白名单挡掉，后者因为非 `://` 形式被要求
恰好是 `owner/repo` 两段。

所以**本地只有一条路：被发现的 catalog**——`$HOME/.agents/plugins/marketplace.json`（个人级）
或从 cwd 往上第一个 `<root>/.agents/plugins/marketplace.json`（仓库级）。本仓库自带后者，
所以 cwd 在仓库内时 `tact-ui plugin marketplace list` 会列出 `auto-memory`。

⚠️ **关键约束（实测，不是推断）**：marketplace 根目录是 git 工作树时，tact 从 **HEAD 的 git
tree** 复制插件（`copy_git_tree`，`install.rs:397-415`），**不是从工作目录**。于是
**未提交的插件目录装不上**：

```
Error: plugin source is absent from revision f0abbc96…: the path 'tact' does not exist in the given tree
```

同一条命令、同一台机器，已提交的 `plugins/agents` 装得上、未提交的 `plugins/tact` 装不上
（`git ls-tree HEAD plugins/` 里只有 `agents`）。→ **发布前必须 commit**；走 git 路线还要 push。
顺带：git tree 复制同样**跳过 symlink**（只写 `filemode != 0o120000` 的 blob），和 §2.2 里
`copy_regular_files` 的结论一致。另外实测：即使 git tree 里记的是 `100755`，装出来的 `.sh`
也是 `644`（拷贝路径不保留 exec 位）——**无影响**，因为 `hooks.json` 用 `sh <path>` 调用它。

**已验证的完整流程**（真实 `tact-ui` 二进制 + 隔离的 `HOME`，cwd 在仓库内）：

```sh
tact-ui plugin marketplace list   # auto-memory: <repo> (catalog: <repo>/.agents/plugins/marketplace.json)
tact-ui plugin install auto-memory-tact@auto-memory
tact-ui plugin list               # auto-memory:auto-memory-tact rev=… [skills=6 hooks]
tact-ui hooks list                # 0 trusted, 2 need review — 标签是 plugin auto-memory-tact
tact-ui hooks trust --source "plugin auto-memory-tact"
```

`hooks list` 的输出确认了两件事：hook 的信任标签就是 `plugin <plugin_id>`，以及未 review 的
hook 确实不会运行——所以安装说明里的 `hooks trust` 不是可选项。

**发布路线**（读代码核对，未实跑）：把仓库推到 git host，用户
`tact-ui plugin marketplace add <owner/repo>` → tact clone 到自己的 marketplace 缓存 → 从
**clone 的 HEAD** 读 `.agents/plugins/marketplace.json` → `./plugins/*` 相对 marketplace 根解析
→ 走同一个 `copy_git_tree`。也就是说"推上去就能装"依赖的正是上面已验证的那条机制。

**个人级安装**（把包暴露给本机，不依赖 cwd）：在 `$HOME/.agents/plugins/marketplace.json` 里写
一条 catalog，`source` 用 `{"source": "local", "path": "./.agents/plugins/auto-memory-tact"}` 形状
——`plugins/agents/README.md` 已给出 agents 版的完整示例，tact 版同理。

**交付状态**：`plugins/tact` 已提交，上面那条命令现在能装成功（实测 `Installed plugin
'auto-memory-tact' from 'auto-memory'`，`hooks list` 显示 2 条待 review，标签 `plugin
auto-memory-tact`）。仍未 push——本地 catalog 走的是 HEAD 的 git tree，所以本地装不需要 push；
要让 `plugin marketplace add <owner/repo>` 那台机器装上才需要。

### 2.6 `hooks.json` 的两处细节

- `matcher` 是**正则**（`matcher_matches`，`hooks.rs:928-931`）：SessionStart 拿
  `startup|resume|compact` 去匹配 tact 报的真实 source，PreCompact/PostCompact 匹配 trigger
  （`manual|auto`），UserPromptSubmit 匹配 prompt 文本。现有写法没问题。
- `timeout` 单位是**秒**，缺省 60，显式 `0` = 不限（`resolve_timeout`，`hooks.rs:693-697`）。
  现在 SessionStart 那条写的是 `30`，可以。
- ✅ 已给 SessionStart 那条加了 **`additionalContextLimit`**（单位 token，`hooks.rs:178-184`）：
  brief 最长 `MAX_BRIEF_CHARS = 10_000`（`profiles.rs`），设成 4000 留了余量，只在索引病态
  膨胀时才会触发截断。

---

## 3. tact-ui 侧要做的事（都是现成命令）

1. 装插件（Tact 装 Tact 那个包，Codex 装 `plugins/agents`）：
   `tact-ui plugin marketplace add <git url 或 owner/repo>` →
   `tact-ui plugin install auto-memory-tact@<marketplace>`。
2. **过信任检查**：`tact-ui hooks list` 会把它列在 "Needs review"，`tact-ui hooks trust`（或
   `--all`）之后才会执行；改过命令就要重新 review（`crates/tact/src/config/cli.rs:126-157`）。
3. MCP 路线（想要 20 个工具而不只是 briefing）：
   `tact-ui mcp add auto-memory-rs --command /abs/path/auto-memory --arg mcp --arg --vault --arg …`。
   默认写项目文件 `<workdir>/.tact/.mcp.json`，加 `--user` 写 `$HOME/.tact/.mcp.json`；
   用 `tact-ui mcp list` / `tact-ui mcp get auto-memory-rs` 核对。建议加 `--read-only`。
4. **不需要**给 tact 加 `[memory]` 配置段——配置属于产品自己（这正是"独立产品"的直接推论）。

---

## 4. 不要做的：把 `auto_memory` 链进 tact-ui

三条独立理由，任一条都够：

1. **许可证**：tact workspace 是 `MIT`，auto-memory 是 `AGPL-3.0-or-later` 且
   [licensing.md](licensing.md) §2 已把"Basic Memory 0.23.2 的衍生作品"写死 → 静态链接后整个
   tact-ui 发行物按 AGPL 分发。copyleft 不看 feature，没有"只给某个模块挂"的解法。
2. **依赖面**：`fastembed` + `ort`（ONNX）现在是**无条件依赖**，库还会把 `tracing-subscriber`
   的默认 feature（`ansi` + `tracing-log`）带进合并后的 feature 集——tact 特意关掉它就是
   为了不让 `log` 桥把 MCP 日志漏进 TUI。
3. **API 形状**：`Store` 不是 `Clone`，`IndexService<'a>` / `McpServer<'a>` 都借 `&mut Store`；
   而 tact 的 hook 闭包要 `'static` 且只拿 `&LoopState` → 得自己捕获 `Arc<Mutex<Store>>`。
   加上 `block_in_place` 在 current-thread runtime（`#[tokio::test]` 的默认）上 panic。

省下的只是一个子进程，换来的是上面三条。不值。

---

## 5. 验收清单

1. `auto-memory doctor --vault … --index … --project …` 先过一遍。
2. 装完插件、trust 之后，起一个会话看 brief 有没有进上下文（Tact 会渲染成
   `<hook-context source="plugin …">` 那一块）。
3. 单条命令就能验，不用起 TUI：
   `echo '{"hook_event_name":"SessionStart","cwd":"/abs/repo","source":"startup"}' | auto-memory hook session-start --harness tact`
4. MCP 路线单独验：`tact-ui mcp get auto-memory-rs` 应打印 transport / source / 工具列表。
5. `--vault` 写错的代价：`mcp` 启动时会按 `--vault` reconcile，路径不对会 prune 掉该项目的
   索引行（integration-guide §2.1）。第一次接完先看 stderr 那行
   `mcp server starting project=… vault=…`。
