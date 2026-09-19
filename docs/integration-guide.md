# auto-memory-rs 接入与使用指南

本文面向"把 `auto-memory-rs` 接到自己的知识库 + 接到 AI 客户端（MCP）"的落地场景。
每一步都可以直接复制执行；标注了本机实测输出，以及在实测中踩出来的坑。

阅读顺序建议：先看 §0 的流程图和 §2 的三个概念（80% 的报错都来自 §2），
然后按 §3 → §7 依次执行。想先弄清"代码怎么分层、数据存哪、检索走了哪些步骤"，
看 [architecture-guide.md](architecture-guide.md)。

> **定位**：Markdown 是唯一真源，SQLite 索引是派生数据（随时可删可重建）。
> 没有 Web UI、没有云同步、没有账号。Obsidian（或任何编辑器）改文件，
> `auto-memory` 索引 + 检索，MCP 负责把这份能力交给 AI 客户端。

---

## 0. 端到端流程

```
① 准备 vault（Markdown 笔记，Obsidian 直接打开这个目录）
        │
② reindex        ──► 生成/增量更新 SQLite 索引（索引放在 vault 外面）
        │
③ status / search / context / schema   ──► 验证索引可用
        │
④ watch（常驻）  ──► Obsidian 一改文件，索引自动跟上
        │
⑤ mcp（常驻）    ──► 客户端（Codex / Claude Code / 任意 MCP 客户端）接入
```

命令一览（全部实测可用）：

| 命令 | 作用 | `--project` 语义 |
|---|---|---|
| `auto-memory reindex` | 建/更新索引 | **项目名**（可省略，默认取 vault 目录名） |
| `auto-memory watch` | 监听 vault 变化并增量索引 | **项目名** |
| `auto-memory mcp` | stdio MCP 服务 | **项目名** |
| `auto-memory status` | 打印索引计数 | **permalink** |
| `auto-memory search` | 全文 / 向量 / 混合检索 | **permalink** |
| `auto-memory context` | 按 `memory://` 走图，输出上下文 | **permalink** |
| `auto-memory schema validate\|infer\|diff` | Picoschema 校验/推断/漂移 | **permalink** |

---

## 1. 前置条件与构建

- Rust 1.85+（`edition = "2024"`）。
- 不需要 Python、不需要 `uv`/`uvx`、不需要数据库服务。
- 语义检索（向量/混合/重排）是可选的，需要 fastembed 模型缓存 + ONNX Runtime；
  文本检索、图上下文、schema、MCP 服务**都不需要**。

```bash
cargo build --release
./target/release/auto-memory --version      # auto-memory 0.1.0
```

可选（只有要用 `--vector` / `--hybrid` / `--reranker` 时才需要）：

```bash
# 模型缓存默认 ~/.config/basic-memory/fastembed_cache（与参考实现共用同一份缓存）
./target/release/auto-memory reindex --vault "$VAULT" --index "$INDEX" --project "$PROJECT" --embeddings
```

下面的示例统一用这组变量，替换成自己的路径即可：

```bash
BIN=/path/to/auto-memory-rs/target/release/auto-memory
VAULT=$HOME/vault                                  # markdown 笔记目录
INDEX=$HOME/.local/share/auto-memory/memory.db       # 索引，放在 vault 之外
PROJECT=oracle                                     # 项目名（见 §2）
```

---

## 2. 三个概念：vault / index / project（最重要的坑）

### 2.1 vault 与 index 必须分开

`--vault` 是 Markdown 目录（Obsidian 打开的那个）；`--index` 是 SQLite 文件，
**放在 vault 外面**，否则它会被 Obsidian 的文件树看到。

- `--index` 的父目录不存在时会自动创建；
- `reindex` 要求 `--vault` 是一个**已存在的目录**，否则直接报错退出
  （`vault directory not found: ...`）；
- `mcp` 和 `watch` 不做这个检查，**而且启动时会按 `--vault` 做一次 reconcile**。
  路径写错时服务照常起来，reconcile 却会认为"vault 里一个文件都没有"，
  于是把该项目已有的索引行全部 prune 掉（实测 `entities: 1 → 0`）。
  Markdown 不受影响，`reindex --full` 即可恢复，但"接上 MCP 发现搜不到"往往就是这个原因——
  **接完 MCP 第一次自检一定要看 vault 路径**（见 §7.1）。

### 2.2 project：写命令传"名字"，读命令传"permalink"

这是最容易出问题的一处。规则：

- `reindex` / `watch` / `mcp` 的 `--project` 是**项目名**；省略时默认取 vault 的目录名。
  项目名会被规范化成 permalink（`generate_permalink`）：`My Vault` → `my-vault`。
- `status` / `search` / `context` / `schema` 的 `--project` 是 **permalink**。

实测：

```bash
# vault 目录名是 "My Vault"，注册出的项目名是 "My Vault"，permalink 是 my-vault
$BIN reindex --vault "$HOME/bm demo2/My Vault" --index /tmp/am-demo/memory2.db
$BIN status --index /tmp/am-demo/memory2.db --project my-vault     # ✅ 正常，打印计数
$BIN status --index /tmp/am-demo/memory2.db --project "My Vault"   # ❌ project not found: My Vault
```

**坑：忘记 `--project` 会在同一个索引里注册出第二个项目。** 同一份 vault、同一个
`memory.db`，先带 `--project demo` 索引一次、再不带 `--project` 索引一次，
`list_memory_projects` 会列出两个项目（`demo` 和目录名 `vault`），都指向同一个路径：

```
{"projects":[{"name":"demo", ...},{"name":"vault", ...}],"default_project":"demo", ...}
```

索引文件可以承载多个项目（多 vault 用同一个 `--index` 是支持的），
但**每条命令都显式写 `--project`**，否则读取侧会找不到你刚写的那个项目。

### 2.3 项目名与 permalink 混用时的行为

`mcp --project my-vault`（传 permalink 而不是名字）能跑，但项目的展示名会被改写成
`my-vault`；`basic_memory_diagnostics` 里能看到实际生效的名字：

```
- Project: my-vault (my-vault)
- Vault: /tmp/bm demo2/My Vault
```

---

## 3. 步骤一：准备 vault（笔记怎么写）

任何 Markdown 目录都行。索引只读文件、**从不改写文件**（这点有测试守着：
`tools/smoke.py` 会在索引前后对 vault 取哈希）。示例：

```markdown
---
title: Ada Lovelace
type: person
tags: [person, history]
---

# Ada Lovelace

- [fact] First programmer
- [role] Mathematician
- works_at [[organizations/analytical-engine]]

See also [[people/grace-hopper]].
```

- `- [category] 内容` → observation（可被 `--category` / `entity_types=observation` 检索）；
- `[[wikilink]]` 和 `relation_type [[目标]]` → relation；目标还不存在时保留为
  unresolved，目标笔记建立后自动解析；
- frontmatter 的 `title` / `type` / `tags` 会进入索引与检索过滤。

索引时自动忽略（`DEFAULT_IGNORE_PATTERNS` + vault 根目录的 `.bmignore`，一行一个
pattern，`#` 开头为注释）：点目录（`.obsidian/`、`.basic-memory/`）、`node_modules`、
非 Markdown 文件。注意是**点目录**被忽略，点文件不会。

---

## 4. 步骤二：建索引

```bash
$BIN reindex --vault "$VAULT" --index "$INDEX" --project "$PROJECT"
```

实测输出（JSON，便于脚本消费）：

```json
{"added":2,"updated":0,"unchanged":0,"skipped":0,"removed":0,"relations_resolved":1}
```

- **增量**：默认只重写变化的文件，重复执行第二次得到 `unchanged: 2`。
- `--full`：清掉陈旧行、重建整个项目（改了忽略规则、或怀疑索引脏了时用）。
- `--embeddings`：额外刷新语义分块与向量（先把文本索引对齐，再做向量）。

---

## 5. 步骤三：验证

```bash
$BIN status  --index "$INDEX" --project "$PROJECT"
$BIN search  --index "$INDEX" --project "$PROJECT" "programmer"
$BIN context memory://people/ada-lovelace --index "$INDEX" --project "$PROJECT" --plain
$BIN schema  validate person --index "$INDEX" --project "$PROJECT" --text
```

实测：`status` → `{"entities":2,"observations":3,"relations":1}`；
`search` 输出 JSON（`results` / `total` / `has_more` / `current_page` / `page_size`），
每条命中带 `title`、`permalink`、`external_id`、`score`、`content`、`file_path`；
`context --plain` 打印图遍历大纲（默认 `--depth 1`、`--timeframe 7d`、
`page 1`、`page-size 10`、`--max-related 10`）。

常用 `search` 过滤（值与 MCP 参数同名，以下均实测）：

```bash
# 实体级（笔记本身）：note_type / tag / permalink（permalink 含项目前缀）
$BIN search --index "$INDEX" --project "$PROJECT" --type person --tag history "programmer"
$BIN search --index "$INDEX" --project "$PROJECT" --permalink "$PROJECT/*" --after-date "2 weeks ago" "programmer"

# observation 级（`- [category] …` 行）：带 --category 时默认只查 observation 行
$BIN search --index "$INDEX" --project "$PROJECT" --entity-type observation --category fact "programmer"

# 过滤条件单独用（不带 query）也成立，例如列全部 person
$BIN search --index "$INDEX" --project "$PROJECT" --type person
```

两个实测出来的注意点：

1. **`--type` 和 `--category` 不能一起用**：`--type` 过滤 entity 行，而出现
   `--category` 时检索默认收敛到 observation 行（与参考实现一致），两者相交必然为空。
   要么用 `--type`（实体级），要么用 `--category` / `--entity-type observation`（observation 级）。
2. **`--title` 可以和 query 一起给**，语义是"标题命中 title 且内容命中 query"：

   ```bash
   $BIN search --index "$INDEX" --project "$PROJECT" --title Ada "programmer"
   # → total 1，demo/people/ada-lovelace
   ```

   MCP 侧 `search_notes{query:"programmer", title:"Ada"}` 同理。
   （参考实现的 MCP 只通过 `search_type="title"` 表达标题检索、并把 query 丢掉；
   本端把 `title` 做成了独立参数，所以两种写法都能用。）

注意 CLI 与 MCP 的一处刻意的格式差异：`--title` / `--permalink` 在这里**带值**
（`--title Alpha`、`--permalink 'projects/*'`），不是开关。
`search` 命令行始终打印 JSON；MCP 的 `search_notes` 默认打印 Markdown
（要 JSON 就传 `output_format="json"`）。

---

## 6. 步骤四：常驻同步（Obsidian 开着的时候）

```bash
$BIN watch --vault "$VAULT" --index "$INDEX" --project "$PROJECT"
```

- 1000 ms 去抖窗口（`--window-ms N` 可改）；
- 把"删除 + 新建"配对成**移动**，所以 Obsidian 文件树里改名不会丢 permalink，
  别的笔记指向它的链接也不会断；Obsidian 的原子保存（临时文件 rename 覆盖）
  算一次更新，不是一次删除加一次创建；
- `--once`：处理一批就退出，适合放进 cron / git hook / 同步工具的钩子。

### 6.1 守护进程怎么看日志

**日志全部走 stderr**（stdout 只留给机器可读的那份 JSON 报告），默认级别 `info`，实测输出：

```
$BIN watch --vault "$VAULT" --index "$INDEX" --project oracle
# stderr：
INFO watching the vault vault=/home/me/vault index=… project=oracle window_ms=1000 once=false
INFO initial reconcile finished added=0 updated=1 unchanged=14 skipped=1 removed=0 relations_resolved=0
INFO watch batch applied label="watch batch" indexed=1 moved=0 removed=0 unchanged=0   ← 有改动才有这行
INFO Ctrl-C received, stopping
INFO watch stopped batches=1
# stdout（只在退出时出现，脚本解析的就是它）：
{ "batches": 1 }
```

| 你想要 | 怎么做 |
|---|---|
| 默认视图（启动 / 初始 reconcile / 每个**有改动**的批次 / 停止） | 什么都不用设 |
| 看到每个文件事件、被忽略的路径（"为什么这个文件没被索引"） | `RUST_LOG=auto_memory=debug` |
| 完全静音，只要那份 JSON | `RUST_LOG=off` |
| 只调某个模块 | `RUST_LOG=auto_memory::indexing=debug`（按 target 过滤） |
| 交给 systemd/journald | 什么都不用做：SIGTERM 现在也是**优雅退出**（冲刷待处理窗口 + 打印 `batches`） |

systemd 示例（`systemctl stop` 发的就是 SIGTERM，日志会进 journal）：

```ini
[Service]
ExecStart=/home/me/.cargo/bin/auto-memory watch --vault %h/vault --index %h/.local/share/auto-memory/memory.db --project oracle
Restart=on-failure
```

---

## 7. 步骤五：接入 MCP 客户端

### 7.1 先手工验一次（不依赖任何客户端）

```bash
$BIN mcp --vault "$VAULT" --index "$INDEX" --project "$PROJECT" <<'EOF'
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"probe","version":"0"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"basic_memory_diagnostics","arguments":{}}}
EOF
```

期望（实测）：

```json
{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"auto-memory-rs","version":"0.1.0"}}}
```

第二个响应里应包含：

```
- Project: oracle (oracle)
- Vault: /home/me/vault
- counts: entities/observations/relations
```

`mcp` 启动时会先按 `--vault` 做一次 **reconcile**（全量扫一遍 vault，按 checksum 增量更新），
所以第一次接客户端**不需要**先手动跑 `reindex`；`counts` 为 0 通常意味着 vault 路径不对
（顺带会把该项目已有索引行 prune 掉，见 §2.1）。
反向的注意：服务进程只在启动时扫一遍，运行期间**不会**感知 Obsidian 的改动——
要么另起 `watch`，要么重启服务。

它的启动信息在 stderr（不影响 stdout 的协议帧纯净）：

```
INFO mcp server starting project=demo permalink=demo vault=/home/me/vault index=… read_only=false
```

**先确认这两行**，再往客户端里配。路径抄错是最常见的"接上了但没数据"。

协议要点：stdio 上跑换行分隔的 JSON-RPC 2.0，协议版本 `2024-11-05`，
服务名 `auto-memory-rs`；stdout 只输出协议帧，诊断信息走 stderr。
支持 `initialize`、`notifications/initialized`、`ping`、`tools/list`、`tools/call`。
Ctrl-C 会等当前请求处理完再退出。

### 7.2 通用客户端配置（JSON）

```json
{
  "mcpServers": {
    "auto-memory-rs": {
      "command": "/home/me/Projects/auto-memory-rs/target/release/auto-memory",
      "args": [
        "mcp",
        "--vault", "/home/me/vault",
        "--index", "/home/me/.local/share/auto-memory/memory.db",
        "--project", "oracle"
      ]
    }
  }
}
```

- Claude Code：用户级配置在 `~/.claude.json` 的 `mcpServers` 下；
  另有官方 CLI `claude mcp add --scope user <name> -- <命令...>`（以 `claude mcp --help` 为准），
  配好用 `claude mcp list` 核对。
- 也可以用项目级配置，但**路径一律写绝对路径**：MCP 客户端的工作目录不是你能预期的。

### 7.3 Codex（`~/.codex/config.toml`）

本机现有的写法是这样（把 command/args 换成 Rust 二进制即可）：

```toml
[mcp_servers.auto-memory-rs]
type = "stdio"
command = "/home/me/Projects/auto-memory-rs/target/release/auto-memory"
args = ["mcp", "--vault", "/home/me/vault",
        "--index", "/home/me/.local/share/auto-memory/memory.db",
        "--project", "oracle"]
```

也有 CLI：`codex mcp add auto-memory-rs -- /path/to/auto-memory mcp --vault ... --index ... --project ...`
（`codex mcp list` / `get` / `remove` 对应增删查）。

> 与官方 Python 版并存：官方版靠 `BASIC_MEMORY_CONFIG_DIR` 发现配置，
> 这个端口**完全靠命令行参数**（不读该环境变量）。所以两条 server 条目必须
> 各自把 vault/index/project 写全；**不要让两个 server 指向同一个 vault 同时写**。

### 7.4 只读接入（推荐用于"只查不改"的客户端）

```bash
$BIN mcp --vault "$VAULT" --index "$INDEX" --project "$PROJECT" --read-only
```

`--read-only` 会把 6 个写工具从 `tools/list` 里隐藏（20 → 14 个），
调用它们返回错误帧：

```json
{"jsonrpc":"2.0","id":3,"error":{"code":-32603,"message":"tool not available in read-only mode: write_note"}}
```

被隐藏的是 `write_note`、`edit_note`、`move_note`、`delete_note`、
`create_memory_project`、`delete_project`。

### 7.5 语义检索（可选）

MCP 侧默认没有向量能力，`search_type="vector"|"semantic"|"hybrid"` 会返回
"Semantic Search Disabled" 指引，而**不会**退化成文本检索。要真跑起来：

```bash
$BIN reindex --vault "$VAULT" --index "$INDEX" --project "$PROJECT" --embeddings   # 先建向量
$BIN mcp --vault "$VAULT" --index "$INDEX" --project "$PROJECT" --model-cache ~/.config/basic-memory/fastembed_cache
```

`--embedding-fixture FILE` 用捕获好的确定性向量（离线测试用），
`--onnx-runtime PATH` 指定 ONNX Runtime 动态库；跨编码器重排默认关闭，
要开就加 `--reranker`（`--reranker-candidates N` 默认 20）。

### 7.6 写入即索引

MCP 的写工具会直接落盘**并同步更新索引**，客户端写完立刻能搜到，不需要再 `reindex`。
实测 `write_note`（`directory="people"`、`note_type="person"`）：

```
# Created note
project: notes
file_path: people/Grace Hopper.md
permalink: notes/people/grace-hopper
checksum: unknown
```

文件名由标题决定（`Grace Hopper.md`），permalink 是 slug（`grace-hopper`）。
随手 `search_notes(query="debugging")` 就能命中。注意 `write_note` 默认
**不能覆盖已存在的笔记**，要覆盖得显式传 `overwrite=true`。

---

## 8. 工具速查表（20 个）

| 分组 | 工具 |
|---|---|
| 笔记 | `write_note` `read_note` `view_note` `read_content` `edit_note` `move_note` `delete_note` |
| 检索 | `search_notes`（另有 ChatGPT 适配器 `search` / `fetch`，按 `initialize` 的 clientInfo 决定是否可用） |
| 图 | `build_context` |
| 目录 | `list_directory` |
| 活动 | `recent_activity` |
| 项目 | `list_memory_projects` `create_memory_project` `delete_project` |
| Schema | `schema_validate` `schema_infer` `schema_diff` |
| 诊断 | `basic_memory_diagnostics` |

要点：笔记类是 upsert 语义（`edit_note` 的 identifier 不存在时会创建笔记）；
所有笔记工具的 `output_format` 默认 `text`，要结构化数据传 `"json"`
（`build_context` 相反：默认 JSON，传 `"text"` 拿 Markdown）。
MCP 服务始终被约束在**一个项目**内，项目生命周期（建/删）用 CLI。
`list_workspaces` 是唯一没实现的参考工具（Web/Cloud 面不在范围内）。

调用示例（可直接粘进 stdio）：

```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"write_note","arguments":{"title":"Grace Hopper","directory":"people","note_type":"person","tags":["person"],"content":"# Grace Hopper\n\n- [fact] Coined 'debugging'\n"}}}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search_notes","arguments":{"query":"debugging"}}}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"build_context","arguments":{"url":"memory://people/grace-hopper","output_format":"json"}}}
```

---

## 9. 排障速查

| 症状 | 原因 | 处理 |
|---|---|---|
| `project not found: <name>` | 读命令用了项目名而不是 permalink | 用 `my-vault` 这种规范化形式；或先 `list_memory_projects` 看 permalink |
| `vault directory not found` | `reindex` 的 `--vault` 不是目录 | 检查路径；`mcp` 不做此检查，别用它来验证 |
| 客户端连上了但搜不到东西 | `--vault` 指向的目录里没有笔记（`mcp` 启动时会 reconcile，不是缺 `reindex`） | 跑 §7.1 的 `basic_memory_diagnostics` 看 Project/Vault/counts；路径写错还会 prune 该项目已有行，改对后用 `reindex --full` 恢复 |
| 同一个 vault 出现两个项目 | 某次命令漏了 `--project`，注册出"目录名"项目 | 每条命令都显式 `--project`；必要时重建索引 |
| `--vector` 结果为空 | 没跑过 `reindex --embeddings` | 先建向量索引；MCP 侧还要 `--model-cache` 或 `--embedding-fixture` |
| `watch --embeddings` 直接退出，退出码 2 | watch 只维护文本索引，不刷新向量；`--embeddings` 是全局开关，不拦就会静默无效 | 向量刷新是单独一趟：`reindex --vault <dir> --index <db> --embeddings` |
| watch 跑着但看不到任何输出，不知道成功没 | 日志在 **stderr**，默认 `info`（启动 / 初始 reconcile / 有改动的批次 / 停止各一行）；stdout 只有退出时的 `{"batches":N}` | 按 §6.1 看 stderr；要事件级细节用 `RUST_LOG=auto_memory=debug` |
| `systemctl stop` 后既没日志也没有 `batches` | 旧版本只监听 SIGINT | 现在 SIGTERM 也优雅退出（冲刷待处理窗口 + 打印 `batches`）；确认跑的是新装的那个二进制 |
| `--type X --category Y` 结果为空 | 前者过滤 entity 行、后者把检索收敛到 observation 行 | 二选一，见 §5 |
| MCP 里语义检索被拒绝 | 未挂载 embedding runtime | 同上；这是刻意行为（不退化成文本检索） |
| 索引写不进去 / 锁等待 | 多进程同时写 | WAL + `busy_timeout=10000`，短暂等待会自愈；避免长事务并发 |
| 客户端拿不到输出 | 往 stdout 里混了日志 | 协议帧只走 stdout，诊断信息在 stderr |

---

## 10. 恢复与重建

Markdown 是唯一真源，所以恢复就是重建派生数据：

```bash
rm "$INDEX"
$BIN reindex --vault "$VAULT" --index "$INDEX" --project "$PROJECT" --full --embeddings
```

全量重建的结果与增量维护的索引一致，包括指向已删除笔记的链接
（会以 unresolved 形式恢复，和文件里的写法一致）。

---

## 11. 与参考实现（Basic Memory 0.23.2）的差异摘要

- 无 Web UI / 云同步 / 账号 / `list_workspaces` / 远程 MCP；
- 索引**从不改写 vault**：参考实现会给缺 frontmatter 的文件注入
  `title`/`type`/`permalink`，这里由编辑器（Obsidian）负责文件内容；
- `read_content` 原样返回图片字节（参考实现会经 Pillow 重编码）；
- `--read-only` 是本端口新增的开关（参考实现没有）；
- 向量用 BLOB 列 + Rust 打分，不用 `sqlite-vec` 的 `vec0` 虚表（结果等价）；
- CLI 的 `--title` / `--permalink` 是带值参数，参考实现是开关；
- 不移植参考 CLI 的 `bm inspect` 检索诊断。

完整清单见 `docs/usage.md` §8，逐条证据见 `docs/reference.md`。
