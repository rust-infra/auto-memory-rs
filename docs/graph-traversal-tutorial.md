# 图遍历教学（真实数据版）

> **这份文档怎么用**：讲清 `build_context` / `memory://` 背后的图遍历——它怎么从一篇笔记出发，
> 一跳一跳把邻居捞出来，以及为什么结果会被截断。数据是参考实现跑出来的真实行。
>
> 配套阅读：[knowledge-graph.md](knowledge-graph.md)（三个概念入门）、
> [glossary.md](glossary.md)（术语）。

---

## 1. 入口：`memory://` 怎么变成起点

```
memory://notes/relations
        │  normalize_memory_url：补前缀、校验字符
        ▼
   notes/relations          （路径）
        │  resolve_entity_path 依次尝试
        │    1. permalink 精确匹配
        │    2. file_path 精确匹配
        │    3. file_path + ".md"
        │    4. permalink 后缀匹配（剥掉项目前缀）
        ▼
   entity id = 15          （种子）
```

解析不到时**返回空结果，不报错**——这是参考实现的行为。

---

## 2. 关键规则：一跳 = 两层

图里只有两种节点：`entity` 和 `relation`。所以从一篇笔记走到"它的邻居"要经过两行：

```
entity(seed)  ──►  relation  ──►  entity(邻居)
   depth 0        depth 1          depth 2
```

于是 API 里的 `depth=1`（"我想看一层邻居"）在 SQL 里是 `max_depth = 1 × 2 = 2`：

```rust
":max_depth": i64::from(depth) * 2,
```

**记住这条，后面所有行号都对得上**：`depth=1` 的结果里会出现 d1（关系）和 d2（实体）两种行。

---

## 3. 递归 CTE 逐段拆解

`src/storage/store.rs` 的 `FIND_RELATED_SQL`：

```sql
WITH RECURSIVE entity_graph AS (
    -- ① 种子：起点实体，depth = 0
    SELECT e.id, 'entity' AS type, e.title, ..., 0 AS depth, e.id AS root_id,
           ',' || e.id || ',' AS entity_path
      FROM entity e
     WHERE e.id IN (seeds) AND e.project_id = :project_id

    UNION ALL

    -- ② 实体 → 关系（每跳第一层）
    SELECT r.id, 'relation' AS type, r.relation_type || ': ' || r.to_name AS title, ...,
           eg.depth + 1,
           CASE WHEN r.from_id = eg.id THEN 0 ELSE 1 END AS is_incoming,   -- 出边还是入边
           eg.entity_path
      FROM entity_graph eg
      JOIN relation r ON (eg.type = 'entity' AND (r.from_id = eg.id OR r.to_id = eg.id))
     WHERE eg.depth < :max_depth

    UNION ALL

    -- ③ 关系 → 实体（每跳第二层）
    SELECT e.id, 'entity' AS type, ..., eg.depth + 1, eg.entity_path || e.id || ','
      FROM entity_graph eg
      JOIN entity e ON (eg.type = 'relation' AND e.id = 对面那一端)
     WHERE eg.depth < :max_depth
       AND instr(eg.entity_path, ',' || e.id || ',') = 0        -- 环检测
)
SELECT DISTINCT ... MIN(depth) AS depth, root_id ...
  FROM entity_graph
 WHERE depth > 0                       -- 去掉种子自己
 GROUP BY type, id, ...                -- 同一个节点可能从多条路径到达
 ORDER BY depth, type, id              -- ← 决定顺序和截断
 LIMIT :max_results;                   -- ← max_related
```

四个设计点：

| 机制 | 作用 |
|---|---|
| `is_incoming`（0/1） | 区分"我指向它"和"它指向我"，遍历时能走对方向 |
| `entity_path` 累加 id | **环检测**：路径上出现过的实体不再进入（`A→B→A` 会被挡住） |
| `GROUP BY ... MIN(depth)` | 同一节点可能经多条路径到达，只保留**最短距离** |
| `root_id` | 每个种子单独成组；同一实体从两个种子出发会出现两行（去重键含 root_id） |

---

## 4. 真实遍历结果

来自 `tests/golden/context/find-related.json`（参考实现的 id 分配）。

### 4.1 `relations-depth1`：种子 = Relations Demo（id 15）

`depth=1` → `max_depth=2` → 9 行，**没被截断**：

| depth | type | id | title |
|---|---|---|---|
| 1 | relation | 2 | `depends_on: projects/alpha` |
| 1 | relation | 3 | `implemented by: people/ada-lovelace` |
| 1 | relation | 4 | `links_to: notes/frontmatter` |
| 1 | relation | 5 | `links_to: projects/beta\|Beta` |
| 1 | relation | 6 | `relates_to: notes/simple` |
| 2 | entity | 7 | simple |
| 2 | entity | 10 | Ada Lovelace |
| 2 | entity | 11 | Beta Project |
| 2 | entity | 13 | Alpha Project |

可以看到：**5 条出边 + 4 个目标实体**。

那第 5 个目标去哪了？`links_to: notes/frontmatter` 其实**解析成功了**（指向 Frontmatter Demo，
`to_id = 14`），但它没出现在 d2。原因是**默认 `timeframe = 7d`**：

```
Frontmatter Demo 的 created_at = 2026-01-02  ← frontmatter 里写了 created
遍历的 since 过滤： e.created_at >= since     ← 被挡在窗口外
```

这是很容易踩的坑：**邻居被 timeframe 过滤掉，但它对应的关系行还在**——所以你会看到一条
指向某实体的关系，却找不到那个实体。去掉 timeframe 或放宽窗口，它就会出现。

### 4.2 `relations-depth2`：同一颗种子，`depth=2` → `max_depth=4`

```json
{"depth": 2, "max_related": 10, "rows": 10}
```

前 9 行和上面完全一样，第 10 行是 **d3 的关系**：

| depth | type | id | title |
|---|---|---|---|
| 3 | relation | 7 | `links_to: projects/alpha` |

——它是"simple（d2）指出去的关系"。**到 10 行就被 `LIMIT` 砍断了**，所以后面更深的实体没进来。

### 4.3 `alpha-depth1`：种子 = Alpha Project（id 13）—— 展示截断

`max_related=10`，正好返回 10 行：

| depth | type | id | title |
|---|---|---|---|
| 1 | relation | 2 | depends_on: projects/alpha |
| 1 | relation | 7 | links_to: projects/alpha |
| 1 | relation | 8 | links_to: projects/alpha |
| 1 | relation | 9 | 关联: projects/alpha |
| 1 | relation | 11 | links_to: notes/simple |
| 1 | relation | 15 | links_to: projects/alpha\|Alpha Project |
| 2 | entity | 3 | Observations Demo |
| 2 | entity | 5 | 中文测试文档 |
| 2 | entity | 7 | simple |
| 2 | entity | 12 | Wikilinks Demo |

拆开看：d1 的 6 条关系里 **5 条是入边**（Relations Demo / simple / Observations Demo / 中文测试文档 /
Wikilinks Demo 指向 Alpha），**1 条是出边**（Alpha → simple，即 id 11 那条 `links_to: notes/simple`）。

d2 本该有 **5 个**实体（每条关系对面那一个），但 `LIMIT 10` 只装得下 4 个——被挤掉的是
**Relations Demo（id 15）**，它的 id 最大、排在 d2 的最后。这就是"同样查询，加不加 `max_related`
结果不一样"的来源。

> 截断是按 `ORDER BY depth, type, id` 后的前 N 行。想多要就加 `--max-related N`
> （上限 `MAX_CONTEXT_RELATED_RESULTS = 100`）。

---

## 5. `build_context` 的响应结构

光是遍历行还不够，`build_context` 会把它们组织成三层：

```json
{
  "results": [{
    "primary_result": { "type": "entity", "title": "Relations Demo", "permalink": "oracle/notes/relations", ... },
    "observations": [],
    "related_results": [ { "type": "relation", "title": "depends_on: projects/alpha", ... },
                         { "type": "entity", "title": "Alpha Project", ... } ]
  }],
  "metadata": {
    "uri": "oracle/notes/relations",
    "depth": 1,
    "primary_count": 1,
    "related_count": 9,
    "total_results": 10,
    "total_relations": 5,
    "total_observations": 5
  },
  "page": 1, "page_size": 10, "has_more": false
}
```

注意几个计数（`relations-depth1` 的真实值）：

| 字段 | 值 | 含义 |
|---|---|---|
| `related_count` | 9 | 遍历返回的相关行数（= §4.1 那 9 行） |
| `total_relations` | 5 | 主笔记**自己写的**关系数 |
| `total_observations` | 5 | 主笔记 + 所有相关实体的 observation 总数（Relations Demo 自己 0 条，4 个邻居共 5 条） |

`related_results` 的条目形态：关系行带 `relation_id` / `relation_type` / `from_entity` / `to_entity` / `to_name`，
实体行带 `entity_id` / `permalink` / `file_path`。

---

## 6. 常见误解 FAQ

**Q：`depth=1` 为什么返回了两种 type 的行？**
A：因为一跳是"实体 → 关系 → 实体"两层，d1 是关系、d2 是实体。见 §2。

**Q：结果为什么比 `max_related` 少？**
A：可能本来就没那么多邻居（如 §4.1 只有 9 行）；也可能是悬空链接——目标没解析时没有对应的实体行。

**Q：同一个实体出现两次？**
A：只有从**不同种子**出发时才会（去重键含 `root_id`）。同一颗种子内靠 `MIN(depth)` 只留一行。

**Q：A 链接 B、B 链接 A，会死循环吗？**
A：不会，`entity_path` 记录了走过的实体 id，回头路会被 `instr(...) = 0` 挡掉。

**Q：为什么从 Alpha 出发看不到 Ada Lovelace？**
A：被 `LIMIT 10` 截断了（§4.3）。加 `--max-related 20` 即可。

**Q：关系行里 `permalink` 为空字符串？**
A：遍历返回的关系行没有 permalink（`''`），实体的 permalink 才带上；需要引用就用实体的。

---

## 7. 复现

```bash
DB=/tmp/demo.db; V=tests/fixtures/vault
auto-memory reindex --full --vault $V --index $DB --project oracle

auto-memory context memory://notes/relations --index $DB --project oracle --depth 1
auto-memory context memory://notes/relations --index $DB --project oracle --depth 2
auto-memory context memory://projects/alpha  --index $DB --project oracle --depth 1
auto-memory context memory://notes/relations --index $DB --project oracle --depth 1 --plain
```

> `--plain` 输出给人看的树状大纲；不加则是参考实现同款的 JSON。

---

## 8. 数据从哪来

| 数据 | 文件 |
|---|---|
| 三组遍历行（含 order 与截断） | `tests/golden/context/find-related.json` |
| `build_context` 完整响应 | `tests/golden/context/*.json`、`*.md`（text 面） |
| 参考 id 分配 | `tests/golden/index/graph-rows.json` |
| SQL 与常量 | `src/storage/store.rs`（`FIND_RELATED_SQL`）、`src/application/context.rs` |

## 9. 相关文档

| 想知道 | 看这里 |
|---|---|
| 三个概念入门（entity/observation/relation） | [knowledge-graph.md](knowledge-graph.md) |
| `build_context` 契约（参数、分页、timeframe） | [context-spec.md](../specs/context-spec.md) |
| 关系怎么从 Markdown 产生 | [data-format.md](data-format.md) §7 |
| 术语 | [glossary.md](glossary.md) |
