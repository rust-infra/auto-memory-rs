# 文档 Review 与待完善清单

- **Review date:** 2026-09-08 (UTC) / 2026-09-09 (local)
- **Reviewed docs:**
  - `docs/basic-memory-rs-spec.md`（规格说明）
  - `docs/basic-memory-rs-execution-plan.md`（执行计划）
- **Reviewer:** Codex
- **Status:** Review complete — gaps logged. **P0 contract docs now delivered** (2026-09-09):
  `reference.md`, `data-format.md`, `search-spec.md`, `context-spec.md`, `mcp-spec.md`,
  `compatibility-spec.md`, `docs/README.md`.
  **Phase 1 golden corpus also delivered (2026-09-10):** `tools/export_reference.py` oracle
  harness, `tests/fixtures/vault`, 51 golden artifacts, `tests/compatibility_helpers.rs`.
  Rust implementation work remains.

## 1. 总体评价

两份文档在**架构方向**上是清晰、正确的：

- Local-only / no Web / no Cloud / Obsidian 工作流边界明确；
- 分层（Adapters → Application → Domain → Infrastructure）与 Rust 组织原则合理；
- "Markdown 为 source of truth、SQLite 可重建"的模型正确；
- 执行顺序（先 spec → corpus → parser → storage → search → MCP）合理；
- golden test、clippy/rustfmt、风险表都有覆盖。

但作为**"核心算法一致"项目的契约**，目前文档偏"原则清单"，缺少可执行、可验证的**具体规范**。最大风险是：参考实现没有 pin，若干关键算法参数只是"should pin"而没有给出值或来源。当前文档能指导"架构长什么样"，还不能指导"算法是否一致"。

## 2. P0 — 不解决就无法谈"算法一致"

### 2.1 参考基线（Reference Baseline）没有固定
- Spec / Plan 都没有记录上游 repository URL、pin 的 commit/tag/release、日期。
- Spec §10 隐含的结论（SQLite FTS5、CJK script n-gram、hybrid `max + bonus*min`）**没有出处、没有验证状态**。如果来自记忆而非实测，必须先验证。
- 需要的产出：`docs/reference.md` —— pinned commit、来源链接、每个关键行为的验证状态（verified / open）。
- 现状最有利的条件：本机已安装官方 `basic-memory`（本地 Python 版 + MCP），且本环境提供同款 MCP 工具。**可以离线把本机官方实现当 oracle 捕获参考行为，不依赖网络。**

### 2.2 执行计划 Phase 0 的交付物还不存在
Plan §2 说 Phase 0 产出：

```text
docs/compatibility-spec.md
docs/data-format.md
docs/search-spec.md
docs/context-spec.md
docs/mcp-spec.md
```

这五份才是"算法一致"的实际载体，目前全部缺失。现两份文档是总纲；下一步应先产出这五份（或其合并版），而不是直接写代码。

### 2.3 Markdown 语法没有精确定义
Spec §8 是能力清单，不是语法契约。缺：
- observation 行的精确语法（`- [category] content`？category 枚举？）与正/反例；
- relation 行的精确语法（`- type [[target]]`、引号规则、默认 `links_to`？）；
- **文件 ↔ Entity 的映射规则**：一个 note 文件是否就是一个 entity？heading 是否生成 entity？prose 中的 wikilink 如何变成 relation？
- permalink 推导规则（来自 frontmatter？文件名？slug 化规则？中文？）；
- 重复 title / permalink 冲突策略；
- 标题行的处理（`# H1` 与 title 的关系）。

### 2.4 MCP 工具契约只有名称
Spec §13 只列了工具名。缺每个工具的：
- 参数名（snake_case）、必填/可选、默认值；
- 输出 JSON 形状与 text 形状；
- 错误文本与错误码；
- `search` vs `search_notes`、`fetch` vs `read_content` 的差异；
- `recent_activity`、`list_directory`、`view_note`、schema 工具的语义。

> 示例（来自本环境实际 MCP 工具定义，可作为捕获起点）：`search_notes` 有 `query`、`search_type`(text/title/permalink/vector/semantic/hybrid)、`page`、`page_size`、`project`、`project_id`、`tags`、`status`、`note_types`、`entity_types`、`categories`、`metadata_filters`、`after_date`、`min_similarity` 等参数；`build_context` 有 `url`、`depth`、`max_related`、`page`、`page_size`、`project`、`output_format`(json/text)、`timeframe`。这些都应逐项写进 mcp-spec。

## 3. P1 — 影响架构落地

### 3.1 配置与项目布局还是 "suggested"
Spec §4 的 `.basic-memory/` 是建议而非决定。需要决定并 pin：
- 索引默认位置（项目内 `.basic-memory/` vs XDG data dir）；
- 配置文件格式与位置（`config.toml`? 全局 vs 项目）；
- 环境变量与 CLI 全局选项（`--project`、`--config` 等）；
- 多项目注册表存在哪里。

### 3.2 并发/多进程模型缺失
- MCP 进程常驻（watcher + SQLite）与 CLI one-shot 可能同时打开同一 SQLite；
- 需要决定：WAL、busy_timeout、跨进程锁、single-instance 策略；
- watcher 触发重建与 CLI 写操作并发时的行为。
- **已解决（2026-09-11，WAL + busy_timeout）**：SQLite 连接档案照参考
  `_configure_sqlite_connection` 整体移植（`src/storage/store.rs::configure_connection`）——
  文件库 `journal_mode=WAL`，以及 `busy_timeout=10000`、`synchronous=NORMAL`、
  `cache_size=-64000`、`temp_store=MEMORY`、`wal_autocheckpoint=1000`（内存库跳过 WAL）。
  移植前 `tests/obsidian_compatibility.rs` 的 `initial reconcile` 会以
  `Sqlite(DatabaseBusy, "database is locked")` 偶发失败（全量跑 10 次复现 1 次）：rollback
  journal 下轮询 reader 跨在 writer 的 commit 上把 writer 饿过超时；WAL 消掉这个机制，
  超时则从 rusqlite 自带的 5 s 抬到参考的 10 s。`tests/sqlite_profile.rs` 钉住。
- **仍未决定**：single-instance 策略，以及「watcher 重建 vs CLI 写」是否需要显式协调
  （目前依赖 SQLite 自身锁 + 上述超时）。

### 3.3 ID 与 golden test 的确定性
- 参考输出含 `external_id`（UUID 风格）。若 MCP/JSON 输出带 ID，golden 比较必须先定义 canonicalization（哪些字段忽略/正则化）。
- 需要决定 ID 生成方式与 schema 版本字段。

### 3.4 向量搜索仍是 open decision
- Spec §10 列了"要 pin 的参数"但没有给默认值（模型、维度、chunk、threshold、top-k、bonus 系数、epsilon）。
- 向量存储/索引方案未定：sqlite-vec vs Rust 内 brute force vs hnsw crate —— 需要选择标准（语料规模、内存、确定性）。
- 模型文件如何获得（安装时下载 vs 运行时下载 vs 自带 fixture 模型）直接影响 offline 测试。

### 3.5 安全与健壮性细节
- symlink 是否跟随、symlink loop；
- path traversal（Spec §14 提了拒绝，但缺实现策略）；
- YAML 安全解析（anchors/aliases、超大文件、非字符串 key）；
- 文件大小/数量上限、hash 算法（sha256?）、原子写与 fsync 策略；
- Obsidian `.trash/` 删除行为。

### 3.6 性能目标未定义
- 目标语料规模（1k / 10k / 100k notes）；
- 全量索引吞吐、增量索引延迟、搜索延迟预算；
- 无目标则 benchmark（Plan §17）无法验收。

## 4. P2 — 可维护性

- **无 README / 文档导航**：`docs/` 下没有索引页，README 也没有指向 spec/plan；
- **无 open questions log / ADR 索引**：多个开放决策（向量方案、配置位置、ID 策略）没有追踪处；
- **无术语表**：permalink vs path vs title、entity vs note、`memory://` 语法、observation category、relation type 需要统一 glossary；
- **日期不一致**：文档头部写 2026-09-08，review 时为 2026-09-09（UTC 2026-09-08T17:21）；应统一为决定日期并记录修订历史；
- **License / 商标**：之前讨论过 AGPL 与命名边界，但没有进 repo（README/LICENSE 未建）；
- **Reference Harness 未定义**：Plan §3 说"保存 reference output"，但没说用哪个官方命令、哪个版本、什么环境、由哪个脚本生成（建议 `tools/export_reference.py` + 环境快照）；
- **进度跟踪**：无状态表标记哪些 phase 完成 / 进行中 / 未开始。

## 5. 建议的优先级执行顺序

```text
P0-1  建立 docs/reference.md：pin 本机官方版本 + 来源 + 验证状态
P0-2  用本机官方实现（离线 oracle）产出 data-format / search / context / mcp 具体 spec
P0-3  定 Markdown grammar 与 文件↔Entity↔permalink 映射规则 + 正反例 fixture 清单
P1-1  定 config / 项目布局 / ID / 并发模型，写进 spec 并记入 open questions
P1-2  定向量方案选择标准与默认参数
P2-1  README + 术语表 + open questions log + 进度表 + 修订历史
```

## 6. 结论

文档**需要继续完善**：架构层已经够用，但契约层（reference pin、data-format、search/context/mcp 具体行为）是空的，而这正是"核心算法一致"的全部意义。建议下一步直接做 P0：把本机已安装的官方 Basic Memory 当作 oracle，离线圈定参考行为，补上五份契约文档；在此之后才开始 Phase 2 的 Rust 骨架。
