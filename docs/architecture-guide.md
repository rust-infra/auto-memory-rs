# auto-memory-rs 架构与数据流

三组图回答三组问题：**代码怎么分层**（§1）、**数据存在哪、长什么样**（§2）、
**一次写入/检索到底走了哪些步骤**（§3、§4）。图为 Mermaid，GitLab/GitHub 直接渲染。

贯穿全文的一条不变量：

> **Markdown 是唯一真源，`memory.db` 是可随时删除并从 vault 重建的派生索引。**
> 写入路径只有一条——改文件（Obsidian 或 MCP 写工具）；索引只有一条出路——从文件重建。

图中常量都来自代码，出处见 §5。

---

## 1. 架构总览

分层规则（`src/lib.rs`）：`adapters → application → domain`。领域层不依赖 CLI、MCP、
SQLite 或具体嵌入运行时；兼容性承诺落在"外部可观测行为"，不落在模块划分上。

```mermaid
flowchart TB
    subgraph OUTER["外部角色"]
        OBS["Obsidian / 编辑器<br/>唯一改文件的一方"]
        AGENT["AI 客户端<br/>Codex / Claude Code / 任意 MCP"]
        HUMAN["人 / 脚本<br/>CLI、cron、git hook"]
    end

    subgraph ADAPTERS["adapters —— 协议翻译"]
        CLI["cli<br/>main.rs 子命令 + Options 解析"]
        MCPSRV["mcp<br/>JSON-RPC 2.0 / NDJSON over stdio"]
        FSA["filesystem"]
    end

    subgraph APP["application —— 用例编排"]
        NOTE["note<br/>写/改/移/删 + 立即重索引"]
        STXT["search_text<br/>检索结果 → text / json / markdown"]
        CTX["context<br/>memory:// 上下文"]
        DIR["directory / activity"]
        SCHAPP["schema / schema_tools"]
    end

    subgraph CORE["领域与格式（无 IO）"]
        DOM["domain<br/>document / entity / observation / relation<br/>permalink / ids / timeframe / dateparser"]
        MDP["markdown<br/>frontmatter / observations / relations<br/>wikilinks / parser / serialize / edit"]
        GRAPH["graph<br/>memory:// 规范化 + 关系遍历 + 实体解析"]
        SCHEMA["schema<br/>Picoschema 解析 / 校验 / 推断 / 漂移"]
    end

    subgraph INFRA["基础设施"]
        IDX["indexing<br/>rebuild / service / watcher / debounce"]
        SEARCH["search<br/>text(FTS5) / vector / hybrid<br/>rerank / index_rows / chunking / relaxation"]
        STORE["storage<br/>schema / store / records"]
        RT["runtime<br/>ONNX embedding + rerank / tokio"]
    end

    subgraph DATA["持久化与外部资源"]
        VAULT[("vault<br/>*.md 真源")]
        DB[("memory.db<br/>SQLite + FTS5")]
        MODELS[("fastembed 模型缓存<br/>bge-small / jina-reranker")]
    end

    OBS --> VAULT
    AGENT -->|"stdio"| MCPSRV
    HUMAN --> CLI
    CLI --> APP
    MCPSRV --> APP
    FSA --> VAULT
    NOTE --> MDP
    NOTE --> IDX
    STXT --> SEARCH
    CTX --> GRAPH
    SCHAPP --> SCHEMA
    MDP --> DOM
    GRAPH --> STORE
    IDX --> MDP
    IDX --> SEARCH
    IDX --> STORE
    SEARCH --> STORE
    SEARCH --> RT
    STORE --> DB
    IDX -->|"只读"| VAULT
    RT --> MODELS
```

几个容易看错的依赖，图中略去了但真实存在：

- `search::vector` 复用 `search::text`：混合检索的 FTS 腿、以及向量腿的行过滤，
  都是直接调 `search_text`（`src/search/vector.rs`）。
- `application::note` 写完文件后回调 `indexing::service`，所以 MCP 的写工具
  不需要外部再跑一次 `reindex`。
- `indexing` 是唯一"读 vault"的路径（`markdown_files` + `load_indexed_document`）；
  `storage` 从不碰文件。

---

## 2. 数据存储

### 2.1 磁盘上有什么

```mermaid
flowchart LR
    subgraph V["vault（真源，只读）"]
        NOTES["notes/**.md<br/>frontmatter + 正文<br/>[category] 观察 + wikilink 关系"]
        BMIGNORE[".bmignore<br/>一行一个 pattern，# 注释"]
    end
    subgraph INDEX["索引（放 vault 之外）"]
        DBF["memory.db<br/>WAL / busy_timeout 10s<br/>父目录不存在会自动创建"]
    end
    subgraph R["运行时资源（可选）"]
        CACHE["~/.config/basic-memory/fastembed_cache<br/>models--qdrant--bge-small-en-v1.5-onnx-q<br/>models--jinaai--jina-reranker-v1-tiny-en"]
        ORT["ONNX Runtime 动态库<br/>--onnx-runtime 或自动探测"]
    end

    V -.->|"reindex --full 重建"| INDEX
    R -.->|"仅 --vector / --hybrid / --reranker / --embeddings 需要"| INDEX
```

忽略规则（`indexing::watcher`）：点**目录**（`.obsidian/`、`.basic-memory/`）、
`node_modules`、非 Markdown 文件，外加 `.bmignore`。点文件不忽略。

### 2.2 表关系

```mermaid
erDiagram
    project ||--o{ entity : "project_id CASCADE"
    entity ||--o{ observation : "entity_id CASCADE"
    entity ||--o{ relation : "from_id CASCADE"
    entity ||--o{ relation : "to_id 可空 = 未解析"
    entity ||--o{ search_vector_chunks : "entity_id"
    search_vector_chunks ||--|| search_vector_embeddings : "rowid = chunks.id"

    project {
        int id PK
        text external_id
        text name "项目名：reindex/mcp 的 --project"
        text permalink "读命令的 --project"
        text path "vault 绝对路径"
    }
    entity {
        int id PK
        int project_id FK
        text external_id "确定性 UUID"
        text title
        text note_type
        text entity_metadata "frontmatter JSON"
        text permalink
        text file_path "vault 相对路径"
        text checksum "SHA-256，增量判据"
        text created_at
        text updated_at
    }
    observation {
        int id PK
        int project_id FK
        int entity_id FK
        text category "来自 [category]"
        text content
        text context
        text tags "JSON 数组"
    }
    relation {
        int id PK
        int project_id FK
        int from_id FK
        int to_id FK "未解析时为 NULL"
        text to_name "wikilink 原文"
        text relation_type
    }
    search_index {
        text id "UNINDEXED 行 id"
        text title "可检索"
        text content_stems "可检索 分词材料"
        text content_snippet "可检索 原文"
        text permalink "可检索"
        text type "UNINDEXED entity/observation/relation"
        text project_id "UNINDEXED"
        text entity_id "UNINDEXED"
        text category "UNINDEXED"
        text metadata "UNINDEXED JSON"
        text updated_at "UNINDEXED"
    }
    search_vector_chunks {
        int id PK
        int entity_id
        int project_id FK
        text chunk_key "type:id:index"
        text chunk_text
        text source_hash "复用判据"
        text entity_fingerprint
        text embedding_model
        text vector_index "blob"
        text embedding_status "pending/ready"
    }
    search_vector_embeddings {
        int rowid PK
        blob embedding "384 维 f32"
        text source_hash
    }
```

`search_index` 是 FTS5 虚表（`tokenize='unicode61 tokenchars 0x2F'`，`prefix='1,2,3,4'`），
列顺序照抄参考实现；`file_path`、`from_id`、`to_id`、`relation_type`、`created_at` 也是
`UNINDEXED` 携带列，图上为省地方没列。

### 2.3 一行 Markdown 落到哪几张表

| Markdown 里的东西 | 落到 | 说明 |
|---|---|---|
| frontmatter `title` / `type` | `entity.title` / `entity.note_type` | `type` 保留作者拼写，比较时走 `normalize_note_type` |
| frontmatter 其它键 / `tags` | `entity.entity_metadata` | 整体存 JSON；检索的 `metadata_filters` 在它上面跑 |
| `- [fact] 内容` | `observation(row)` | `category=fact`，`content=内容` |
| `relation_type [[目标]]`、`[[目标]]` | `relation(row)` | `to_name` 存原文；目标不存在则 `to_id` 为 `NULL`（unresolved） |
| 文件本身 | `entity.file_path` + `checksum` | checksum 决定"这次要不要重写索引" |
| ↳ 每一条 entity/observation/relation | 一条 `search_index` 行 | `content_stems` 由**标题/正文/permalink/文件路径/tags 的变体**拼成（`search::index_rows`） |
| 正文（仅 `--embeddings`） | `search_vector_chunks` + `search_vector_embeddings` | 900 字符切块、120 重叠；向量按 `source_hash` 复用 |

三个身份标识，别混：

- `project.name` = 写命令的 `--project`（未规范化，如 `My Vault`）；
- `project.permalink` = 读命令的 `--project`（规范化，如 `my-vault`）；
- `entity.external_id` = `deterministic_uuid(project_permalink + file_path)`，
  所以**重建索引、原子保存都不会换 id**；`move_file` 会把 destination 按原 id 重写，
  改名因此保住 permalink 与入链。

---

## 3. 写入与索引流程

### 3.0 索引什么时候被刷新

先给结论，再给流程图。索引是派生数据，**只有下面这几个入口会写它**：

| 触发 | 走的路径 | 范围与行为 |
|---|---|---|
| `reindex`（默认） | `IndexService::reconcile()` | 增量：逐文件比 SHA-256，相同则 `unchanged` 不重写；新/变则重解析并重写该文件的所有行；**walk 没看到的 `file_path` 会被删除（prune）**；收尾统一 `resolve_relations` |
| `reindex --full` | `rebuild_vault()` | 全量：不等 checksum，逐文件重写（`replace_document` 覆盖，entity id 保持）；prune 陈旧行；写 `parser_version` 标记 |
| `watch` | 启动时**先做一次全量 reconcile**，再进事件循环 | 循环内按事件做单文件 `index_file` / `remove_file` / `move_file`（1000ms 去抖，见 §3.2） |
| `mcp` | 启动时 reconcile 一次（`ensure_project` → `reconcile()`） | 首次接客户端不必先手动 `reindex`；之后每个写工具调用后 `force_index_file`（见 §3.3）。**进程运行期间不感知外部改动** |
| `reindex --embeddings` | 先 reconcile 文本索引，再重建向量 | 向量是另一套派生数据：`source_hash` 未变的 chunk 复用已存向量；**普通 `reindex` 不动向量**（见 §3.4） |
| 索引文件被删 / 首次运行 | `Store::open` 建表 | 得到空索引，内容要等下一条上述命令才补上 |

不会触发刷新的：`status` / `search` / `context` / `schema` 只读 store；也没有"发现 vault
变旧就自动重建"的逻辑。**主动全量扫 vault 的只有三处**：`reindex`、`watch` 启动、`mcp` 启动。

两个衍生结论：

- `reconcile` 的 prune 按 `--project` 定位，所以用**写错的 `--vault` + 同一个 `--project`**
  启动 `mcp`/`watch`，会把该项目所有索引行删掉（实测 `entities: 1 → 0`）。Markdown 不受影响，
  `reindex --full` 即可恢复。
- 常驻进程与外部编辑的分工：`watch` 负责"文件变了就更新"，`mcp` 只负责"启动时对齐 + 写工具后同步"。
  两者同时指向一个 vault 没问题（索引的写操作由 SQLite 的 WAL + `busy_timeout=10s` 串行化），
  但**别指望 `mcp` 自己发现 Obsidian 的改动**。

### 3.1 `reindex`（全量 / 增量）

```mermaid
flowchart TB
    A["reindex --vault V --index DB --project P"] --> B["Store::open(DB)<br/>建父目录 + 建表 + WAL + busy_timeout"]
    B --> C{"--vault 是目录?"}
    C -->|否| C1["报错退出：vault directory not found"]
    C -->|是| D["upsert_project(name → permalink)"]
    D --> E{"--full ?"}
    E -->|否 reconcile| F["walk vault<br/>忽略点目录 / node_modules / 非 md / .bmignore"]
    E -->|是 rebuild| G["先 prune 陈旧行"]
    G --> F
    F --> H["逐文件：SHA-256 ↔ entity.checksum"]
    H -->|相同| I["Unchanged"]
    H -->|新增 / 变化| J["解析 frontmatter + 正文"]
    J --> K["markdown::parser<br/>observations / relations / wikilinks"]
    K --> L["storage::replace_document（单事务）<br/>entity upsert + 重写 observation/relation/search_index 行"]
    L --> M["解析关系目标<br/>命中则填 to_id，未命中留 NULL"]
    I --> N
    M --> N["报告 added / updated / unchanged / skipped / removed / relations_resolved"]
    F -->|"文件已消失"| O["remove_document（级联清理）"]
    O --> N
```

要点：

- 增量与全量走同一条流水线，全量多一步 prune；两者结果收敛（`tests/incremental_golden.rs`）。
- malformed frontmatter 的文件计入 `skipped`，不会中断整轮。
- **索引从不改写 vault**：这条路径只读文件。

### 3.2 `watch`（Obsidian 开着时的实时同步）

运行期日志走 stderr，默认 `info` 一行一个批次（`RUST_LOG=auto_memory=debug` 加事件级与忽略原因，
`RUST_LOG=off` 静音）；stdout 只在退出时给一份 `{"batches":N}`。SIGINT 与 SIGTERM 都是优雅退出：
先冲刷待处理窗口再报数（`indexing::watcher::log_watch_report`、`main.rs::shutdown_signal`）。

```mermaid
flowchart TB
    A["watch --vault V --index DB --project P"] --> A1["启动时先全量 reconcile 一遍<br/>（报告 reconciled，见 §3.0）"]
    A1 --> B["notify 监听 vault"]
    B --> C["map_notify_event → (相对路径, Create/Modify/Delete/Rename)"]
    C --> D{"命中忽略规则?"}
    D -->|是| Z["丢弃"]
    D -->|否| E["Debouncer：每路径安静 1000ms"]
    E -->|"tick 100ms 检查到期"| F["成批处理"]
    F --> G{"同一批里<br/>delete + create 同指纹?"}
    G -->|是| H["move_file<br/>保持 entity id / permalink / 入链"]
    G -->|否| I{"事件类型"}
    I -->|create/modify| J["index_file<br/>checksum 相同则 Unchanged"]
    I -->|delete| K["remove_file<br/>级联删 observation / relation / 检索行"]
    H --> L["WatchReport：indexed / unchanged / moved / removed"]
    J --> L
    K --> L
    L -->|"--once 则退出，否则继续"| B
```

Obsidian 的原子保存（临时文件 rename 覆盖）被识别为**一次更新**，不是删除 + 新建；
文件树里的改名被识别为**移动**，所以 permalink 和别人的 wikilink 都不断。

### 3.3 MCP 写工具（写入即索引）

```mermaid
flowchart LR
    A["mcp 启动"] --> A1["ensure_project<br/>注册项目 + 全量 reconcile 一次"]
    A1 --> B["tools/call write_note | edit_note | move_note | delete_note"]
    B --> C["application::note<br/>NoteService"]
    C --> D["markdown::serialize<br/>frontmatter 合并 + 原子写（临时文件 + rename）"]
    D --> E["force_index_file<br/>同一个文件直接重索引"]
    E --> F["storage::replace_document 事务"]
    F --> G["返回 file_path / permalink / checksum<br/>+ 文本摘要（默认 output_format=text）"]
```

所以 MCP 写完立刻能搜到，不需要外部 `reindex`；`application::note` 是文件与索引之间
唯一的写入口，两边不会漂移。但 startup 那次 reconcile 之后再没有"扫 vault"的动作：
运行期间用 Obsidian 改了文件，要么等下次重启，要么另起 `watch`。

### 3.4 向量索引（`reindex --embeddings`）

```mermaid
flowchart TB
    A["reindex --embeddings"] --> B["先 reconcile：文本索引对齐 vault"]
    B --> C["semantic_rows<br/>取本项目全部 search_index 行（按 type, id）"]
    C --> D["chunking：900 字符 / 120 重叠<br/>chunk_key = type:id:index"]
    D --> E{"source_hash 未变?"}
    E -->|"是（复用）"| F["沿用已存向量"]
    E -->|否| G["ONNX bge-small-en-v1.5<br/>384 维、CLS pooling、归一化"]
    G --> H["写 search_vector_chunks + search_vector_embeddings（BLOB）<br/>embedding_status = ready"]
    F --> H
    H --> I["EmbeddingReport：chunks / reused / embedded"]
```

向量存 BLOB、在 Rust 里算余弦，而不是 `sqlite-vec` 的 `vec0` 虚表：两者都是精确 KNN，
结果与参考实现逐行相等，区别只落在 `vector_index` 标记（`blob`）上。

---

## 4. 数据检索流程

入口映射先摆清楚（同一个引擎，不同外壳）：

| 入口 | 走哪条路 |
|---|---|
| `search <query>` / `search_notes(query, search_type="text")` | §4.1 文本 |
| `search --vector` / `search_type="vector"｜"semantic"` | §4.2 向量 |
| `search --hybrid` / `search_type="hybrid"` | §4.3 混合 |
| `search --reranker`（或 MCP 加 `--reranker`） | §4.1–4.3 之后追加 §4.4 |
| `context <memory://…>` / `build_context` | §4.5 图遍历 |
| `recent_activity`、`list_directory` | 走存储直查 + 同一套 hydration（本节不含） |

### 4.1 文本检索（FTS5）

```mermaid
flowchart TB
    A["query + 过滤条件"] --> B["prepare_fts_query<br/>分词 / 引号 / 布尔 / 前缀 *"]
    B --> C["build_filters：收集 MATCH 谓词 + 列过滤"]
    C --> C1{"MATCH 谓词几条?"}
    C1 -->|"1 条"| C2["保持历史形态<br/>文本腿用 (title OR content_stems OR content_snippet)"]
    C1 -->|"≥2 条"| C3["折叠为一条表级 MATCH<br/>{title} : (…) AND {permalink} : (…)"]
    C2 --> D["COUNT + SELECT（bm25 打分）"]
    C3 --> D
    D --> E["过滤：project_id / type / note_type / tag / status<br/>category / metadata_filters / permalink / after_date"]
    E --> F{"排序"}
    F -->|"有 MATCH"| F1["score ASC（bm25 为负，升序即最相关）<br/>带 after_date 时 score ASC, updated_at DESC"]
    F -->|"无 MATCH（纯过滤）"| F2["updated_at DESC，score = 0.0"]
    F1 --> G{"总数 = 0 且是文本查询?"}
    F2 --> G
    G -->|是| H["relaxation：放宽重试一次<br/>（引号/布尔拒绝、三词下限、停用词等）"]
    G -->|否| I["取一页（page_size + 1 判 has_more）"]
    H --> I
    I --> J["hydration：补 entity permalink / external_id"]
    J --> K["结果行：title / type / score / permalink / content / metadata"]
```

两个实测坑写在流程里：`--type`（entity 行）与 `--category`（默认收敛到 observation 行）
相交必为空；`title` 腿与文本腿原来会撞出 FTS5 的 "unable to use function MATCH…"，
现在由上面那条"≥2 条就折叠"分支处理。

### 4.2 向量检索

```mermaid
flowchart TB
    A["query 文本"] --> B["embed_query<br/>bge-small → 384 维"]
    B --> C["候选窗口<br/>无重排：max((page_size+1+offset)×10, 100)<br/>有重排：max(100, candidates×4) + tail×10<br/>tail = max(0, page_size+1+offset − candidates)"]
    C --> D["逐 chunk 算余弦<br/>chunk_key 解析出 type:id"]
    D --> E{"score ≥ min_similarity?"}
    E -->|否| E1["丢弃（默认 0.55）"]
    E -->|是| F["每个检索行只留最佳 chunk<br/>并列按 chunk_key 定序"]
    F --> G["截断到候选窗口（≤ MAX_VECTOR_K 4096）"]
    G --> H{"请求带行过滤?"}
    H -->|是| H1["filter-only 扫描交叉<br/>（FTS 查询不带文本，扫描上限 50000 行）"]
    H -->|否| I
    H1 --> I["按 type 过滤 + 补 entity 信息"]
    I --> J{"有重排?"}
    J -->|否| K["按 page_size + 1 切页"]
    J -->|是| L["交给 §4.4"]
    K --> M["matched_chunk：正文 ≤2000 字符给全文<br/>否则给最佳 5 个 chunk，用 --- 连接"]
    L --> M
    M --> N["结果（total_is_exact = false：向量分页不算总数）"]
```

### 4.3 混合检索（FTS + 向量 融合）

```mermaid
flowchart TB
    A["query"] --> V["向量腿：见 §4.2 的候选与打分"]
    A --> T["FTS 腿：search_text(query + 同一套过滤)"]
    T --> T1["归一化 FTS 分数<br/>取绝对值 / 最大值，过 gate（< 0 记 0）"]
    V --> U["行键统一为 (type, id)<br/>—— 裸 id 在三种行类型之间会撞"]
    T1 --> U
    U --> F["fuse_hybrid：<br/>score = max(v, f) + 0.3 × min(v, f)<br/>只有一条腿的行保留该腿分数"]
    F --> O["排序：分数降序；并列时 FTS 序在前，再是向量独有序"]
    O --> P{"有重排?"}
    P -->|否| Q["按 page_size + 1 切页"]
    P -->|是| R["交给 §4.4"]
    Q --> S["hydration（向量腿命中时带上 matched_chunk）"]
    R --> S
```

融合公式与常量照抄参考实现：`max + 0.3 × min`，版本标记 `max+0.3*min/v1`。
向量腿在融合窗口外还多取 10 倍 chunk（无重排时）以便融合有足够候选。

### 4.4 重排（可选支路，默认关闭）

```mermaid
flowchart LR
    A["候选池（融合/向量结果）"] --> B["取固定前缀<br/>reranker_candidates 默认 20"]
    B --> C["构造文档：body + 换行 + title<br/>截断到 2000 字符"]
    C --> D["jina-reranker-v1-tiny-en 交叉编码打分"]
    D --> E["用新分数替换原分数<br/>尾部按 floor/(index+2) 逐级降级"]
    E --> F["切页（在重排之后切片，不是之前）"]
```

参考实现默认关闭（首次运行要下模型、且增加延迟），本端一致：只有 `--reranker`
或 `--reranker-fixture` 才启用。

### 4.5 上下文检索（图遍历）

```mermaid
flowchart TB
    A["memory://notes/simple 或 recent_activity"] --> B["normalize_memory_url"]
    B --> C["resolve_entity_path<br/>顺序：permalink → file_path → 去掉 .md → 项目前缀 + 裸路径"]
    C --> D{"解析到实体?"}
    D -->|否| D1["空结果 + metadata 仍回显解析出的 uri<br/>（不是错误）"]
    D -->|是| E["find_related：递归 CTE<br/>depth×2 次逻辑跳，visited 去重<br/>ORDER BY depth, type, id LIMIT max_related"]
    E --> F["时间窗过滤：timeframe 解析为 since 边界"]
    F --> G["hydration：主实体 + observations + 相关行摘要 + metadata 计数"]
    G --> H{"输出"}
    H -->|"--plain"| H1["CLI 轮廓"]
    H -->|"text"| H2["MCP markdown"]
    H -->|"json（默认）"| H3["参考实现的结构化 payload"]
```

遍历顺序由 `ORDER BY depth, type, id` 决定，`max_related` 截断因此是有语义的；
这条 SQL 与参考实现逐字一致（含 `LIMIT`），改它就会改 golden。

`recent_activity` 复用同一套 hydration：它等价于"没有 `memory_url` 的 build_context"，
先按 `updated_at DESC` 取最近的实体再走同一段遍历。

---

## 5. 常量与出处

| 常量 | 值 | 位置 |
|---|---|---|
| 最小余弦相似度 | `0.55` | `search::vector::DEFAULT_MIN_SIMILARITY` |
| 向量候选基准 / 硬上限 | `100` / `4096` | `DEFAULT_VECTOR_K` / `MAX_VECTOR_K` |
| 混合融合 | `max + 0.3 × min`（`max+0.3*min/v1`） | `search::vector::FUSION_BONUS` |
| FTS gate | `0.0` | `FTS_GATE_THRESHOLD` |
| 向量腿过滤扫描上限 | `50000` 行 | `VECTOR_FILTER_SCAN_LIMIT` |
| chunk 长度 / 重叠 | `900` / `120` 字符 | `search::chunking` |
| 小笔记全文阈值 / 命中 chunk 数 | `2000` / `5` | `SMALL_NOTE_CONTENT_LIMIT`、`TOP_CHUNKS_PER_RESULT` |
| 重排窗口 / 池放大 / 文档截断 | `20` / `×4` / `2000` | `runtime::rerank` |
| 嵌入模型 / 维度 | `BAAI/bge-small-en-v1.5`（onnx-q）/ `384` | `search::embedding` |
| 重排模型 | `jinaai/jina-reranker-v1-tiny-en` | `runtime::rerank` |
| watch 去抖 / tick | `1000ms` / `100ms` | `indexing::watcher` |
| SQLite | WAL、`busy_timeout=10000`、`synchronous=NORMAL`、`cache_size=-64000`、`wal_autocheckpoint=1000` | `storage::store` |
| FTS5 分词 | `unicode61 tokenchars 0x2F`，`prefix='1,2,3,4'` | `storage::schema` |
| schema 版本 | `1` | `storage::schema::SCHEMA_VERSION` |
| context 默认 | depth `1`、timeframe `7d`、page `1`、page-size `10`、max-related `10` | `main.rs::context_command` |

---

## 6. 想继续往下读

| 主题 | 文档 |
|---|---|
| 安装、索引、MCP 接入、排障 | [integration-guide.md](integration-guide.md)（中文）、[usage.md](usage.md) |
| Markdown 格式契约（frontmatter / observation / permalink / FTS 行模型） | [data-format.md](data-format.md) |
| 检索行为契约（FTS5、向量、融合、过滤、分页） | [search-spec.md](search-spec.md) |
| `build_context` / `memory://` / `recent_activity` | [context-spec.md](context-spec.md) |
| MCP 工具清单、参数、响应模型 | [mcp-spec.md](mcp-spec.md) |
| 参考实现基线与逐条证据 | [reference.md](reference.md) |
| 设计取舍与刻意不用的模式 | [patterns.md](patterns.md) |
