# 检索流程与文本 / Hybrid 召回分析

> 本文分析 `auto-memory-rs` 当前的检索实现，重点覆盖：
> 请求分派、FTS5 文本召回、BM25、向量召回、Hybrid 融合、归一化、候选窗口、Rerank 和分页边界。
>
> 配套流程：
> [交互式检索请求生命周期](visuals/search-retrieval-flow.html)
> · [图定义](visuals/search-retrieval-flow.sequence.json)
>
> 相关教学文档：
> [检索全流程教学](search-pipeline-tutorial.md)
> · [向量化全流程](vector-pipeline-tutorial.md)
> · [架构指南](architecture-guide.md)

---

## 1. 总体结论

检索不是一个统一引擎，而是三条共享过滤、hydration 和分页逻辑的召回路径：

| 模式 | 主要排序信号 | 总数语义 | 典型入口 |
|---|---|---|---|
| 文本 | FTS5 `bm25()` | 精确 | `search_type=text/title/permalink` |
| 向量 | cosine similarity | 非精确 | `search_type=vector/semantic` |
| Hybrid | 固定融合公式 | 非精确 | `search_type=hybrid` |
| 精排 | cross-encoder relevance | 继承上游路径 | 显式启用 Rerank |

总体流程：

```text
MCP search_notes / CLI search
        │
        ├─ 解析 query 与过滤条件
        ├─ 决定 search_type
        │
        ├─ text / title / permalink ──> FTS5
        ├─ vector / semantic ─────────> 向量召回
        └─ hybrid ────────────────────> FTS + 向量融合
                                              │
                                     可选 Rerank
                                              │
                                    分页、格式化和返回
```

主要入口：

| 功能 | 源码 |
|---|---|
| MCP `search_notes` | `src/adapters/mcp/server.rs` |
| CLI `search` | `src/main.rs` |
| 文本召回 | `src/search/text.rs` |
| Query 改写 | `src/search/query.rs` |
| 宽松重试 | `src/search/relaxation.rs` |
| 向量与 Hybrid | `src/search/vector.rs` |
| Rerank | `src/search/rerank.rs` |
| FTS5 索引结构 | `src/storage/schema.rs` |
| 索引行构造 | `src/search/index_rows.rs` |

---

## 2. 检索数据从哪里来

Markdown 文档先被解析为：

- `entity`
- `observation`
- `relation`

然后派生到 FTS5 虚表 `search_index`。

关键字段：

| 字段 | 是否索引 | 作用 |
|---|---:|---|
| `title` | 是 | 标题匹配 |
| `content_stems` | 是 | 兼容命名，实际是标题、正文、路径和标签的文本变体 |
| `content_snippet` | 是 | 正文匹配与结果预览 |
| `permalink` | 是 | permalink 匹配 |
| `file_path` | 否 | 过滤和返回 |
| `type` | 否 | entity / observation / relation 过滤 |
| `project_id` | 否 | 项目隔离 |
| `entity_id` | 否 | 关联 owning entity |
| `category` | 否 | observation 分类过滤 |
| `metadata` | 否 | tag 和 note type 等过滤 |
| `updated_at` | 否 | 时间过滤和 recency 排序 |

FTS5 tokenizer：

```sql
tokenize='unicode61 tokenchars 0x2F'
prefix='1,2,3,4'
```

`content_stems` 不是 stemming。实现会拼入：

- 原文
- 小写形式
- 路径段
- 单词
- 正文
- permalink 变体
- 文件路径变体
- tags

不同行类型的来源文本不同：

- Entity：标题、正文、permalink、文件路径、tags
- Observation：observation content，tags 放在 metadata
- Relation：关系标题，例如 `A -> B`

---

## 3. 文本召回

文本路径的核心函数是 `search_text`。

```text
请求
  ↓
构造 TextSearchOptions
  ↓
严格 FTS5 查询
  ↓
count + SELECT + BM25
  ↓
严格结果为零？
  ├─ 否 → 返回
  └─ 是 → 判断是否允许 relaxed OR
              ├─ 否 → 返回空
              └─ 是 → 再执行一次 count + SELECT
  ↓
hydration
  ↓
精确分页
```

### 3.1 请求预处理

MCP 默认 `search_type=text`。

`search_type` 的主要映射：

| search_type | 实际行为 |
|---|---|
| `text` | query 搜索 title、content_stems、content_snippet |
| `title` | query 映射到 title 字段 |
| `permalink` | 精确 permalink 或含 `*` 的 glob |
| `vector` / `semantic` | 向量召回 |
| `hybrid` | FTS + 向量融合 |

如果请求没有 query，`search_type` 不参与模式选择，只执行过滤型文本查询。

如果没有 query，也没有任何过滤条件，则直接返回 `# No Search Criteria`。

默认 `entity_types` 是 `entity`。如果仅提供 `categories`，默认类型会变为 `observation`，因为 category 只存在于 observation 行。

### 3.2 Query 改写

普通文本 query 由 `prepare_fts_query` 转成 FTS5 表达式。

| 用户输入 | FTS5 表达式 |
|---|---|
| `rust` | `rust*` |
| `emoji unicode` | `emoji* AND unicode*` |
| `zzzz-not-present` | `"zzzz-not-present"*` |
| `rust AND architecture` | `rust* AND architecture*` |
| `notes/foo.md` | quoted path，不加尾随 `*` |

主要规则：

- 普通词添加 prefix wildcard。
- 多词默认使用 AND。
- Boolean operator 会被保留。
- 带空格、标点或路径语义的内容可能被转换成 phrase。
- title 查询不加 prefix wildcard。
- 引号会转义，避免用户输入直接成为 FTS5 语法。

### 3.3 MATCH 的字段范围

普通正文查询搜索三个字段：

```sql
title MATCH ?
OR content_stems MATCH ?
OR content_snippet MATCH ?
```

如果同时存在正文 query、title 或简单 permalink MATCH，代码会把多个 MATCH 合并成一个 table-level MATCH：

```text
search_index MATCH ?
```

其中每个条件使用列过滤表达作用范围：

```text
{title content_stems content_snippet} : (rust*)
AND
{title} : (Alpha)
```

这样做的原因是 FTS5 不允许 OR 连接的 MATCH 组直接和第二个独立 MATCH 拼接。

需要注意：

- `title` 过滤是 FTS token 匹配，不是严格字符串相等。
- `permalink` 精确条件仍使用 SQL `=`。
- `permalink_match` 含 `*` 时使用 SQL `GLOB`。
- `permalink_match` 含 `/` 时通常使用精确匹配。
- 简单 permalink_match 可能转换成 FTS MATCH。

### 3.4 过滤条件

过滤条件在文本路径中直接下推到 SQL，而不是先召回再过滤。

主要过滤：

| 过滤器 | 实现方式 |
|---|---|
| `entity_types` | `search_index.type IN (...)` |
| `note_types` | `json_extract(search_index.metadata, '$.note_type')` |
| `categories` | `search_index.category IN (...)` |
| `tags` | entity metadata 或 observation metadata 的 JSON 检查 |
| `status` | owning entity 的 `entity_metadata` 子查询 |
| `metadata_filters` | owning entity 的 JSON 字段子查询 |
| 精确 permalink | `search_index.permalink = ?` |
| permalink glob | `search_index.permalink GLOB ?` |
| `after_date` | `datetime(updated_at) > datetime(?)` |

实现边界：

- metadata key 只接受 ASCII 字母、数字和下划线。
- 非法 metadata key 会被静默忽略。
- 当前 `tags` 过滤实际只使用 `tags[0]`，不是完整的多标签 AND / OR。
- `after_date` 使用 `updated_at`，不是索引创建时间。

### 3.5 计数、评分和排序

`run_query` 首先执行精确计数：

```sql
SELECT count(*)
FROM search_index
WHERE ...
```

然后执行结果查询：

```sql
SELECT
    ...,
    bm25(search_index) AS score
FROM search_index
WHERE ...
ORDER BY score ASC
LIMIT ? OFFSET ?
```

SQLite FTS5 的 BM25：

- 通常是负数。
- 值越小，越相关。
- `ORDER BY score ASC` 让更相关的结果排在前面。

如果没有 MATCH，只有过滤条件：

```sql
SELECT
    ...,
    0.0 AS score
FROM search_index
WHERE ...
ORDER BY updated_at DESC
```

即：

- score 固定为 `0.0`
- 按最近更新时间排序
- 不计算无意义的 BM25

如果同时存在 MATCH 和 `after_date`：

```sql
ORDER BY score ASC, updated_at DESC
```

也就是先按相关性，再按更新时间。

### 3.6 宽松重试

只有严格查询返回零结果时，才可能执行 relaxed retry。

重试条件不是单纯的 query 长度，而是 query 的形状：

- 含引号：不重试
- 显式 Boolean query：不重试
- 普通拉丁 query 少于 3 个 token：不重试
- 存在 numeric token：不重试
- CJK query 从 2 个 whitespace 词开始允许
- stopwords 会先被删除
- 带 apostrophe 的 term 会转换成 quoted prefix

示例：

```text
zzz-nothing-matches-this
→ zzz* OR nothing* OR matches*
```

但：

```text
rust AND arch
→ 不重试
```

### 3.7 Hydration 和分页

FTS 查询返回 `search_index` 的必要列后，还需要：

1. 把 `type` 解析成 `entity | observation | relation`。
2. 使用 `entity_id` 查询实体信息。
3. 补齐 `permalink` 和 `external_id`。
4. 把 `content_snippet` 截断到最多 4000 字符。
5. 根据类型填充 `entity_id`、`observation_id`、`relation_id`。
6. 文本结果不设置 `matched_chunk`。
7. 根据 offset 和结果长度计算 `has_more`。

文本结果：

```text
total_is_exact = true
```

分页使用：

```sql
LIMIT page_size OFFSET (page - 1) * page_size
```

---

## 4. BM25

BM25 全名通常写作：

**Best Match 25** / **Best Matching 25**

完整名称：

**Okapi BM25**

其中：

- BM 表示 Best Match / Best Matching。
- 25 表示 Okapi 检索系统中的版本或迭代编号。

BM25 的主要参考信号：

- 查询词在文档中的词频
- 查询词在整个语料中的逆文档频率
- 文档长度
- 平均文档长度

SQLite FTS5 中的 `bm25(search_index)` 返回相关性排名值，不是概率。当前实现保留其负数原样，并使用升序排序。

---

## 5. 向量召回的角色

Hybrid 的向量腿由 `ranked_matches` 完成：

1. 读取项目和模型对应的 ready chunks。
2. 计算 query vector 与每个 chunk 的 cosine similarity。
3. 取 top-k chunk。
4. 按 `(type, id)` 聚合。
5. 同一结果行保留最高相似度。
6. 应用 `min_similarity`。
7. 在有过滤条件时，通过 filter-only FTS 扫描做交集。
8. 根据允许的 `entity_types` 做最终类型过滤。

向量候选最终表示成：

```rust
Vec<(SearchKey, f32)>
```

其中：

```rust
SearchKey {
    item_type: "entity" | "observation" | "relation",
    id: ...,
}
```

使用 `(type, id)` 是因为不同表的 id 会冲突。

---

## 6. Hybrid 检索

Hybrid 的主函数：

```rust
pub async fn search_hybrid(
    store: &Store,
    project_id: i64,
    text: &str,
    query_vector: &[f32],
    model: &str,
    options: &VectorSearchOptions,
    rerank: Option<&RerankRequest<'_>>,
) -> Result<SearchPage>
```

它不是简单拼接两条最终页面，而是：

```text
计算较大的候选窗口
  → 向量腿召回
  → FTS 腿召回
  → 两条腿统一成 (type, id)
  → FTS 分数归一化
  → 融合
  → hydration
  → 可选 Rerank
  → 当前页切片
```

### 6.1 分页和探测

Hybrid 先计算：

```text
page = max(input_page, 1)
page_size = max(input_page_size, 1)
offset = (page - 1) * page_size
limit = page_size + 1
```

多取一行用于判断 `has_more`。

如果最终返回了 `page_size + 1` 行，就说明还有下一页，然后截断到 `page_size`。

### 6.2 候选窗口

没有 Rerank 时：

```text
candidate_window = max(100, (page_size + 1 + offset) * 10)
```

有 Rerank 时：

```text
prefix = max(100, reranker_candidates * 4)
tail = max(0, page_size + 1 + offset - reranker_candidates)
candidate_window = prefix + tail * 10
```

向量 chunk 池：

```text
有 Rerank：vector_chunk_pool = candidate_window
无 Rerank：vector_chunk_pool = candidate_window * 10
```

也就是说，没有 Rerank 时，Hybrid 会为向量腿准备更大的 chunk 候选池；最终再截到融合窗口。

### 6.3 向量腿

```rust
let vector_matches = ranked_matches(
    store,
    project_id,
    query_vector,
    model,
    vector_chunk_pool,
    options.min_similarity,
).await?;
```

随后：

- 应用行过滤
- 应用类型过滤
- 截到 `candidate_window`
- 转换成 `(SearchKey, vector_score)`

#### 6.3.1 Chunk 到结果行的聚合为什么会出现重复 key

向量索引的最小检索单位是 chunk，但最终返回单位是搜索结果行。因此
`aggregate_matches` 必须执行一次多对一聚合：

```text
多个 chunk key
    ↓ 去掉 chunk index
一个 `(type, id)` 结果行 key
```

chunk key 的格式是：

```text
type:id:index
```

例如：

```text
entity:7:0
entity:7:1
entity:7:2
```

这些 chunk key 本身都是唯一的，但转换后：

```rust
entity:7:0  → SearchKey { item_type: "entity", id: 7 }
entity:7:1  → SearchKey { item_type: "entity", id: 7 }
entity:7:2  → SearchKey { item_type: "entity", id: 7 }
```

所以：

```text
chunk key 唯一
row key 不唯一
```

`SearchKey` 同时包含 `item_type` 和 `id`，因此不同表里相同 id 也不会冲突：

```text
entity:7:0       → (entity, 7)
observation:7:0  → (observation, 7)
relation:7:0     → (relation, 7)
```

`RowMatch` 保存聚合后的结果：

```rust
pub struct RowMatch {
    pub key: SearchKey,
    pub score: f32,
    pub chunks: Vec<(f32, String)>,
}
```

其中：

- `key`：结果行 key，例如 `(entity, 7)`。
- `score`：该结果行目前看到的最佳 chunk 相似度。
- `chunks`：该结果行命中的所有 chunk 证据，每一对是 `(chunk 相似度, chunk 文本)`。

字段使用复数 `chunks`，是因为同一结果行可以有多个 chunk：

```text
entity:7:0 ─┐
entity:7:1 ─┼─> RowMatch(entity, 7)
entity:7:2 ─┘
```

聚合循环使用：

```rust
if !rows.contains_key(&key) {
    order.push(key.clone());
    rows.insert(
        key.clone(),
        RowMatch {
            key: key.clone(),
            score,
            chunks: Vec::new(),
        },
    );
}
```

这个判断不是为了防止数据库主键重复，而是为了在多个 chunk 映射到同一个结果行时，
只初始化一次 `RowMatch`。

第一次遇到 `(entity, 7)`：

- 把这个 key 写入 `order`。
- 在 `rows` 中创建空的 `chunks` 列表。

后续再次遇到 `(entity, 7)`：

- 不再向 `order` 重复添加 key。
- 不再覆盖已有 `RowMatch`。
- 更新最佳 score。
- 把当前 chunk 追加到 `chunks`。

后续逻辑是：

```rust
if let Some(row) = rows.get_mut(&key) {
    if score > row.score {
        row.score = score;
    }
    row.chunks.push((score, chunk_text));
}
```

如果没有 `contains_key` 判断，`rows.insert` 会不断覆盖之前的 `RowMatch`，
导致前面的 chunk 证据丢失，同时 `order` 中也可能出现重复结果 key。

#### 6.3.2 为什么最后使用 `rows.remove`

聚合结束后有两个结构：

```rust
order: Vec<SearchKey>
rows: HashMap<SearchKey, RowMatch>
```

- `order` 保存第一次出现的稳定顺序。
- `rows` 保存真正的结果对象。

HashMap 不保证遍历顺序，因此不能直接：

```rust
rows.into_values().collect()
```

代码使用：

```rust
order
    .into_iter()
    .filter_map(|key| rows.remove(&key))
    .collect()
```

按 `order` 的顺序取出结果。

这里的 `remove` 不是删除数据库数据，而是：

```text
从本地 HashMap 中取出 RowMatch
并把它移动到最终的 Vec
```

`remove` 返回：

```rust
Option<RowMatch>
```

因此使用 `filter_map`：

- `Some(row)`：移动到输出。
- `None`：跳过。

正常路径中 `order` 的每个 key 都一定存在于 `rows`，所以不会出现 `None`。
使用 `remove` 而不是 `get().cloned()` 的原因是避免克隆：

```rust
chunks: Vec<(f32, String)>
```

中的全部 chunk 文本。`remove` 直接转移所有权，输出时没有额外复制。

如果不用 `filter_map`，等价写法是：

```rust
let mut results = Vec::new();
for key in order {
    if let Some(row) = rows.remove(&key) {
        results.push(row);
    }
}
results
```

### 6.4 FTS 腿

Hybrid 复用文本搜索：

```rust
let mut fts_options = options.filter_options();
fts_options.query = Some(text.to_owned());
fts_options.page = 1;
fts_options.page_size = candidate_window;

let fts_page = store.search_text(project_id, &fts_options).await?;
```

因此 FTS 腿包含：

- query 改写
- FTS5 MATCH
- 过滤条件
- BM25
- relaxed OR fallback

### 6.5 FTS 分数归一化

原始 BM25 是负数且没有固定上界，不能直接与 cosine 比较。

当前实现：

```text
maximum = max(|bm25_i|)
fts_norm_i = |bm25_i| / maximum
```

如果 maximum 为 0：

```text
fts_norm_i = 0
```

示例：

```text
原始 BM25: [-4.0, -2.0, -1.0]
绝对值:    [ 4.0,  2.0,  1.0]
最大值:     4.0
归一化:    [ 1.0,  0.5,  0.25]
```

含义：

- 最相关结果映射为 1.0。
- 其余结果表示相对于当前窗口最强结果的占比。
- 该值不是概率。
- 该值不是跨窗口稳定的绝对相关度。
- 如果窗口只有一个 FTS 结果，它会直接变成 1.0。

### 6.6 融合公式

核心公式：

```text
fused = max(fts, vector) + 0.3 * min(fts, vector)
```

源码：

```rust
pub const FUSION_BONUS: f32 = 0.3;

let score = fts_score.max(vector_score)
    + FUSION_BONUS * fts_score.min(vector_score);
```

解释：

- `max(fts, vector)`：保留两路中更强的一路，单通道结果不会因另一路为零而被平均压低。
- `min(fts, vector)`：只有两路都比较高时才会高，因此表示双路一致性。
- `0.3`：双路一致性奖励系数。
- 双路都接近 1.0 时，融合分数最高接近 1.3。
- 融合分数不是概率，可以大于 1。

示例：

| FTS | Vector | max | min | fused |
|---:|---:|---:|---:|---:|
| 0.8 | 0.7 | 0.8 | 0.7 | 1.01 |
| 0.9 | 0.2 | 0.9 | 0.2 | 0.96 |
| 1.0 | 0.0 | 1.0 | 0.0 | 1.00 |
| 0.0 | 0.8 | 0.8 | 0.0 | 0.80 |
| 0.8 | 0.8 | 0.8 | 0.8 | 1.04 |

融合后按分数降序排列。同分时：

1. 先保留 FTS 腿的顺序。
2. 再保留只在向量腿出现的顺序。

### 6.7 Hydration：把轻量候选补全为 SearchResult

`hydrate_entries` 位于召回排序和 API 返回之间。向量腿和 Hybrid 融合阶段只需要
`(type, id)`、score 和 matched chunks 就能完成排序，但调用方最终需要标题、
permalink、文件路径、内容、metadata 等完整字段。

```text
召回阶段
  HydrationEntry { key, score, chunks, fallback_to_content }
        ↓
hydrate_entries
        ↓
API 返回阶段
  SearchResult { title, permalink, content, metadata, ... }
```

源码入口：

```rust
async fn hydrate_entries(
    store: &Store,
    project_id: i64,
    entries: Vec<HydrationEntry>,
) -> Result<Vec<SearchResult>>
```

#### 6.7.1 输入结构

```rust
struct HydrationEntry {
    key: SearchKey,
    score: f32,
    chunks: Vec<(f32, String)>,
    fallback_to_content: bool,
}
```

字段含义：

| 字段 | 含义 |
|---|---|
| `key` | `(type, id)`，定位 `search_index` 行 |
| `score` | cosine 分数或 Hybrid 融合分数 |
| `chunks` | 该结果行命中的 chunk：`(相似度, chunk 文本)` |
| `fallback_to_content` | 没有可用 chunk 时，是否允许尝试 content snippet 回退 |

`SearchKey` 同时包含 `item_type` 和 `id`，因为不同表可能存在相同的数字 id：

```text
entity:7
observation:7
relation:7
```

#### 6.7.2 第一步：收集并去重数字 id

```rust
let mut ids: Vec<i64> = entries.iter().map(|entry| entry.key.id).collect();
ids.sort_unstable();
ids.dedup();
```

这里只收集数字 id，再排序和去重，目的是减少数据库查询参数。

例如：

```text
entries:
  entity:7
  observation:7
  entity:8

ids before dedup: [7, 7, 8]
ids after dedup:  [7, 8]
```

虽然 `7` 同时来自 entity 和 observation，但这里只是按数字 id 批量读取
`search_index`，不会在数据库查询前丢掉类型信息。完整区分在下一步完成。

#### 6.7.3 第二步：批量读取 search_index

```rust
let rows = store.search_rows_by_ids(project_id, &ids).await?;
```

返回的是 `Vec<SearchRowView>`，每个视图包含：

- `id`
- `title`
- `item_type`
- `permalink`
- `file_path`
- `content_snippet`
- `metadata`
- `entity_id`
- `category`
- `relation_type`
- `updated_at`

然后把数据库行转换成：

```rust
HashMap<(String, i64), &SearchRowView>
```

key 是：

```text
(item_type, id)
```

之所以不能只按 id 建 lookup，是因为同一个数字 id 可能对应多个类型。

#### 6.7.4 第三步：按召回顺序遍历 entries

```rust
for entry in entries {
    let Some(row) = lookup.get(&(entry.key.item_type.clone(), entry.key.id)) else {
        continue;
    };
```

这里遍历的是 `entries`，不是数据库返回的 `rows`。

因此 `hydrate_entries` 本身不会重新排序，而是保持输入 `entries` 的顺序。需要特别区分：

```text
未启用 Rerank：
  entries 顺序
  = 最终搜索结果顺序

启用 Rerank：
  hydrate_entries 仍按 Rerank 前的 entries 顺序生成 SearchResult
  → 随后 rerank_and_paginate 再重排这些 SearchResult
  = 最终搜索结果顺序
```

也就是说，Rerank 的顺序不是由 `hydrate_entries` 保持的，而是在它之后由
`rerank_and_paginate` 产生的。

#### 6.7.4.1 什么是“向量相似度顺序”

向量-only 路径的 `entries` 来自 `ranked_matches`。排序过程大致是：

```text
读取全部 ready chunks
  ↓
计算 query vector 与每个 chunk 的 cosine
  ↓
按 chunk similarity 降序
  ↓
按 (type, id) 聚合
  ↓
同一结果行保留最高相似度
  ↓
按结果行最高相似度降序
  ↓
形成 HydrationEntry 顺序
```

顺序的主排序键是：

```text
vector_score DESC
```

例如：

```text
HydrationEntry 顺序：
  entity:7     0.82
  entity:3     0.74
  observation:9 0.68
  relation:2   0.61
```

如果 chunk 分数相同，底层会先用稳定的 chunk key / owning entity id 规则确定 chunk 顺序；
聚合行后的分数仍然相同时，行顺序保持第一次遇到该结果的顺序。

所以“向量相似度顺序”就是：

> 按最终结果行的最佳 cosine similarity 从高到低排列。

它只描述向量腿内部的相关性顺序，不包含 BM25，也不包含 Hybrid 融合分数。

#### 6.7.4.2 什么是“Hybrid fused score 顺序”

Hybrid 路径不会直接使用向量腿顺序或 FTS 腿顺序，而是：

```text
FTS 原始 BM25
  ↓ normalize_fts_scores
归一化 FTS 分数
  ↓
与 vector cosine 按 (type, id) 合并
  ↓ fuse_hybrid
fused = max(fts, vector) + 0.3 * min(fts, vector)
  ↓
按 fused score 降序
  ↓
形成 HydrationEntry 顺序
```

例如：

```text
entity:7       fused = 1.01
entity:3       fused = 0.88
observation:9  fused = 0.68
entity:11      fused = 0.61
```

主排序键是：

```text
fused_score DESC
```

同分时的稳定顺序是：

1. 先按 FTS 腿中的出现顺序。
2. 再按只在向量腿中出现的顺序。

所以“Hybrid fused score 顺序”就是：

> 两条召回腿融合后的最终相关性分数从高到低排列。

它已经是 Hybrid 用于进入 Hydration 的顺序，但在启用 Rerank 时还不是最终输出顺序。

#### 6.7.4.3 什么是“Rerank 后的顺序”

Rerank 发生在 hydration 之后：

```text
HydrationEntry 顺序
  ↓
hydrate_entries
  ↓
完整 SearchResult 列表
  ↓
rerank_and_paginate
  ↓
Rerank 后的顺序
```

Rerank 会：

1. 取固定候选池，默认通常是前 `reranker_candidates` 条。
2. 用 cross-encoder 为 pool 中的每条结果重新打分。
3. 用 relevance score 替换原 score。
4. 对 pool 按新分数稳定降序排列。
5. 对未进入 pool 的 tail 按 `floor / (index + 2)` 降分，并保持在 pool 后面。
6. 最后按 offset / limit 切片。

例如 Rerank 前：

```text
1. entity:7       vector/fused = 0.91
2. entity:3       vector/fused = 0.87
3. observation:9  vector/fused = 0.80
```

Rerank 后可能是：

```text
1. observation:9  rerank = 0.93
2. entity:7       rerank = 0.71
3. entity:3       rerank = 0.52
```

所以“Rerank 后的顺序”是：

> cross-encoder 重新打分后产生的最终精排顺序。

它与“向量相似度顺序”和“Hybrid fused score 顺序”不是同一层概念：

| 顺序 | 生成阶段 | 主排序键 | 是否最终顺序 |
|---|---|---|---|
| 向量相似度顺序 | 向量召回聚合后 | cosine DESC | 未启用 Rerank 时是 |
| Hybrid fused score 顺序 | FTS / 向量融合后 | fused DESC | 未启用 Rerank 时是 |
| Rerank 后的顺序 | hydration 之后精排 | cross-encoder score DESC | 启用 Rerank 时是 |

#### 6.7.4.4 三种顺序是否互斥

这里需要区分两个完全不同的层面：

1. **最终排序规则**
2. **检索流水线阶段**

结论是：

```text
作为最终排序规则：互斥
作为流水线阶段：不互斥，存在前后依赖
```

##### 向量相似度顺序与 Hybrid fused score 顺序

这两个顺序对应不同 `search_type`：

```text
search_type = vector
  → 只运行向量召回
  → 使用向量相似度顺序

search_type = hybrid
  → 同时运行 FTS 和向量
  → 产生 Hybrid fused score 顺序
```

因此从“本请求最终使用哪套排序规则”看，它们是互斥的：

```text
一个请求不会同时以向量相似度顺序和 Hybrid fused score 顺序作为最终顺序
```

但它们并不是两个完全独立的结果来源。Hybrid 内部必须使用向量腿：

```text
Hybrid
  ├─ 向量腿：cosine similarity
  ├─ FTS 腿：BM25
  └─ 合并后：Hybrid fused score
```

所以从流水线阶段看：

```text
Hybrid fused score 顺序依赖向量相似度信息
但不会把向量腿的原始顺序直接当作最终顺序
```

结果集合也可能不同：

- 向量-only 只包含向量召回结果。
- Hybrid 还包含只在 FTS 中命中的结果。
- 两条路径的结果可以重叠，但不保证相同。

##### Hybrid fused score 顺序与 Rerank 顺序

这两个不是互斥关系，而是顺序执行关系：

```text
FTS + Vector
    ↓
Hybrid fused score 顺序
    ↓
hydrate_entries
    ↓
rerank_and_paginate
    ↓
Rerank 后的顺序
```

Hybrid fused score 顺序负责：

1. 决定哪些结果进入候选窗口。
2. 未启用 Rerank 时，直接作为最终顺序。
3. 启用 Rerank 时，决定前 `reranker_candidates` 条进入精排池。
4. 决定未进入 Rerank pool 的 tail 顺序。
5. 为 Rerank 提供稳定输入。

Rerank 负责：

1. 对固定头部候选重新打分。
2. 用 cross-encoder relevance 替换这些行的原分数。
3. 重新排列头部候选。
4. 保持 pool 整体位于 tail 之前。
5. 最后执行分页切片。

因此：

```text
未启用 Rerank：
  最终顺序 = Hybrid fused score 顺序

启用 Rerank：
  Hybrid fused score 顺序 = Rerank 的输入
  Rerank 后的顺序 = 最终顺序
```

可以说 Rerank 覆盖了 Hybrid 融合顺序在头部 pool 中的排序，但没有消除它：

- pool 成员仍由 Hybrid 顺序决定。
- tail 仍保持 Hybrid 顺序并执行降分。
- 启用 Rerank 前，HydrationEntry 仍按 Hybrid 顺序生成。

##### 向量-only 和 Rerank

向量-only 路径同样可以启用 Rerank：

```text
向量相似度顺序
  ↓
hydrate_entries
  ↓
rerank_and_paginate
  ↓
Rerank 后的顺序
```

因此 Rerank 不是 Hybrid 专属顺序，而是向量和 Hybrid 两条语义召回路径之后都可选的精排步骤。

##### 三种关系的完整图

```text
vector-only
    │
    ▼
向量相似度顺序
    │
    ├──────────────► 未启用 Rerank：最终顺序
    │
    └──────────────► 启用 Rerank
                         │
                         ▼
                    Rerank 后的顺序


hybrid
    │
    ▼
Hybrid fused score 顺序
    │
    ├──────────────► 未启用 Rerank：最终顺序
    │
    └──────────────► 启用 Rerank
                         │
                         ▼
                    Rerank 后的顺序
```

##### 一句话判断

如果问题是：

```text
一个请求最终用哪一种排序规则？
```

答案是：

```text
三种最终排序规则互斥
```

如果问题是：

```text
这些顺序在实现中会不会同时出现？
```

答案是：

```text
会。它们可以出现在同一条流水线的不同阶段，后一阶段依赖前一阶段。
```

如果 `search_index` 中找不到对应行，就跳过。常见原因包括：

- 索引和数据不同步
- 文档刚被删除
- 索引正在重建
- 向量 chunk 的 key 已经过期

跳过是静默的，因此实际返回条数可能少于输入 entry 数量。

#### 6.7.5 第四步：整理 chunk 和 matched_chunk

先按 chunk 相似度重新排序：

```rust
let mut chunks = entry.chunks;
chunks.sort_by(|left, right| right.0.total_cmp(&left.0));
```

然后丢弃分数，只提取文本：

```rust
let texts: Vec<String> = chunks.into_iter().map(|(_, text)| text).collect();
```

接着调用：

```rust
let matched_chunk = matched_chunk_text(snippet, &texts).or_else(...);
```

`matched_chunk_text` 的规则：

| 条件 | `matched_chunk` |
|---|---|
| `content_snippet` 非空且不超过 2000 字符 | 返回完整 snippet |
| 长文本且存在 chunk | 返回最相关的 5 个 chunk |
| 没有 chunk 且没有可用 snippet | `None` |
| 长文本且没有 chunk | 当前实现通常得到 `None` |

多 chunk 使用：

```text

---

```

连接，顺序是相似度降序。

#### 6.7.6 fallback_to_content 的真实行为

Hybrid 会设置：

```rust
fallback_to_content: true
```

向量-only 路径设置：

```rust
fallback_to_content: false
```

但回退条件当前是：

```rust
(entry.fallback_to_content && !texts.is_empty())
    .then(|| snippet.map(str::to_owned))
    .flatten()
```

这意味着回退还要求 `texts` 非空。

因此需要区分两种情况：

```text
短 FTS-only 行：
  snippet <= 2000
  matched_chunk_text 已经返回 snippet
  fallback 不会成为决定因素

长 FTS-only 行：
  snippet > 2000
  texts 为空
  matched_chunk_text 返回 None
  fallback 因 !texts.is_empty() 不成立
  最终 matched_chunk = None
```

所以注释里“row without chunks reports its content snippet”在当前代码中并不完全成立。
对于长文本且没有向量 chunk 的 FTS-only Hybrid 行，最终不会返回 `matched_chunk`。

#### 6.7.7 第五步：补全 owning entity

```rust
let hydration = entity_hydration(store, row.entity_id).await?;
```

它会查询 owning entity：

- `entity.permalink`
- `entity.external_id`

返回值是：

```rust
(Option<String>, Option<String>)
```

如果 `entity_id` 为空：

```text
(None, None)
```

如果 entity 查不到：

```text
(None, None)
```

这一部分不会因为 entity 缺失而跳过结果行，只会缺少 entity 级别的 permalink 和 external id。

#### 6.7.8 第六步：构造 SearchResult

最终字段来源如下：

| `SearchResult` 字段 | 来源 |
|---|---|
| `title` | `search_index.title`，缺失时为空字符串 |
| `item_type` | `search_index.type` 解析成枚举 |
| `score` | `HydrationEntry.score` |
| `entity` | owning entity 的 permalink |
| `external_id` | owning entity 的 external id |
| `permalink` | `search_index.permalink` |
| `content` | `content_snippet`，最多 4000 字符 |
| `matched_chunk` | snippet 或 top chunks |
| `file_path` | `search_index.file_path` |
| `metadata` | `search_index.metadata` JSON 解析 |
| `entity_id` | `search_index.entity_id` |
| `observation_id` | observation 类型时为 `search_index.id` |
| `relation_id` | relation 类型时为 `search_index.id` |
| `category` | `search_index.category` |
| `from_entity` | 当前 helper 固定为 `None` |
| `to_entity` | 当前 helper 固定为 `None` |
| `relation_type` | `search_index.relation_type` |
| `updated_at` | `search_index.updated_at` |

需要注意：

- `metadata` 是 JSON；解析失败时静默变成 `None`。
- `content` 单独截断到 4000 字符，但不影响匹配时使用的完整 FTS 索引文本。
- `from_entity` 和 `to_entity` 在这个 helper 中没有补全，始终为 `None`。

#### 6.7.9 一个完整的转换示例

召回输入：

```rust
HydrationEntry {
    key: SearchKey {
        item_type: "entity",
        id: 7,
    },
    score: 0.82,
    chunks: [
        (0.82, "Rust project uses SQLite"),
        (0.71, "Hybrid search fuses FTS and vectors"),
    ],
    fallback_to_content: false,
}
```

`search_index` 中查到：

```text
title = "Rust Search Design"
permalink = "notes/rust-search"
file_path = "notes/rust-search.md"
content_snippet = "A long note..."
metadata = {"note_type":"reference"}
entity_id = 7
```

最终输出：

```rust
SearchResult {
    title: "Rust Search Design",
    item_type: SearchItemType::Entity,
    score: 0.82,
    permalink: Some("notes/rust-search"),
    content: Some("A long note..."),
    matched_chunk: Some("Rust project uses SQLite
---
Hybrid search fuses FTS and vectors"),
    file_path: "notes/rust-search.md",
    metadata: { "note_type": "reference" },
    entity_id: Some(7),
    ..
}
```

#### 6.7.10 顺序、缺失和重复

顺序：

- 输出顺序由 `entries` 决定。
- `search_rows_by_ids` 的数据库返回顺序不会影响最终排序。
- `HashMap` 只用于查找，不用于决定结果顺序。

缺失：

- `search_index` 缺失：跳过整个结果。
- `entity` 缺失：保留结果，但 entity 字段为空。
- `metadata` 非法：保留结果，但 metadata 为 `None`。

单次请求内重复：

- 输入 `entries` 已经按 `(type, id)` 聚合，正常情况下没有重复。
- 如果数据库里同一 `(type, id)` 出现多个 `search_index` 行，lookup 会覆盖前一个，
  因此当前实现不会返回同一 key 的多个副本。

跨页重复：

- `hydrate_entries` 不保存跨请求状态。
- 它无法判断某个 `(type, id)` 是否已经在前一页返回过。
- 因此分页期间数据变化仍可能造成跨页重复。

#### 6.7.11 复杂度

设：

```text
E = HydrationEntry 数量
U = 去重后的 search_index id 数量
C = 所有 entry 的 chunk 总数
```

主要成本：

| 操作 | 复杂度 / 查询数 |
|---|---|
| 收集和排序 ID | `O(E log E)` |
| 批量读取 search_index | 1 次数据库查询 |
| 建立 lookup | `O(U)` |
| 遍历 entries | `O(E)` |
| chunk 排序 | 约 `O(C log C)` |
| entity hydration | 每个 entry 1 次查询，页级 N+1 |

因此数据库查询次数的近似模型是：

```text
1 次 search_rows_by_ids
+ 有 entity_id 的 entry 数量次 entity 查询
```

#### 6.7.12 可优化点

1. 把 `entity_hydration` 改为批量查询。

```text
收集 entity_id
→ entity_permalinks_and_external_ids(&all_entity_ids)
→ 内存 lookup
→ 填充每个 SearchResult
```

2. 修正 `fallback_to_content` 条件。

如果目标确实是“没有 chunk 时使用长文本 snippet”，条件不应要求
`!texts.is_empty()`。需要先确认参考实现的准确语义，再补测试。

3. 考虑让 `HydrationEntry` 使用具名 chunk 结构。

```rust
struct ChunkMatch {
    score: f32,
    text: String,
}
```

这样可以避免大量 `chunk.0` / `chunk.1`。

4. 为缺失行、非法 metadata 和长文本 FTS-only fallback 增加明确测试。

目前这些边界主要依赖代码阅读，而不是显式测试固定下来。

### 6.8 Rerank：为什么需要、如何排序及分页

Rerank 是可选的第二阶段排序。整个检索系统采用：

```text
第一阶段：召回候选
第二阶段：对少量候选精排
```

FTS、向量和 Hybrid 都属于第一阶段；Rerank 属于第二阶段。

#### 6.8.1 为什么第一阶段不够精确

##### 向量检索使用双编码器，但编码算法相同

“双编码器”或“bi-encoder”描述的是**前向计算结构**：query 和 document 分别经过编码器，
得到两个独立向量，再进行向量比较。它不代表 query 和 document 使用不同的编码算法。

在当前项目中，query 和 document 实际使用同一个 embedding provider、同一个模型和同一条
推理路径：

```text
document text → encoder(shared weights) → document vector
query text    → encoder(shared weights) → query vector

score = cosine(query vector, document vector)
```

`EmbeddingProvider::embed_query` 的默认实现也是直接调用
`embed_documents(&[query])`，因此这里确实是同一套编码算法。

需要区分的是：

```text
编码算法：相同
模型参数：通常相同
前向计算：各自独立
模型内部交互：没有 query/document 的联合注意力
```

因此 query 和 document 最终会落在同一个 embedding 空间，可以比较；但编码时它们不会
互相“看到”对方。

它的优势是 document embedding 可以提前计算，查询时只需做向量比较，速度快。

缺点是 query 和 document 没有在模型内部一起阅读，因此难以精确判断：

- query 的具体意图是否被回答
- 条件与限定关系
- 否定关系
- 数字、版本和范围
- 多个词组合后的真正含义
- “主题相似”与“真正相关”的区别

例如：

```text
query:
  "不支持云同步的本地 Rust 方案"

候选 A:
  "Rust 本地存储，不需要云同步"
  → 真正相关

候选 B:
  "Rust 项目需要云同步"
  → 主题接近，但实际不相关
```

两者的向量相似度都可能较高，因为 embedding 主要捕获整体语义方向，而不是完整回答关系。

##### BM25 只理解词法匹配

BM25 擅长：

- 精确关键词
- 路径
- 标识符
- 版本号
- 组合词

但同义词和改写可能无法命中，例如：

```text
query: "offline storage"
document: "local persistence without network"
```

它本身不具备跨词语义理解能力。

##### Hybrid 仍然是固定公式

Hybrid 使用：

```text
max(fts, vector) + 0.3 * min(fts, vector)
```

它改善的是两条召回腿之间的结果覆盖和初步排序，但它仍然只是对两个检索分数做固定融合。

Hybrid 能解决更多召回问题，但不保证候选之间的最终相关性顺序完全正确。

##### 双编码器和 Cross-encoder 最直观的区别

可以用一句最简化的话概括：

```text
双编码器：
  query 和 document 分开看，再比较

Cross-encoder：
  query 和 document 一起看，再打分
```

双编码器：

```text
query    → encoder → query vector
document → encoder → document vector

score = cosine(query vector, document vector)
```

Cross-encoder：

```text
[query + document]
        ↓
   cross-encoder
        ↓
 relevance score
```

双编码器可以理解成：

```text
先分别给 query 和 document 写一份“语义摘要”
再比较两份摘要像不像
```

Cross-encoder 可以理解成：

```text
把 query 和 document 交给模型一起阅读
模型直接判断这篇 document 是否真的回答了 query
```

##### 为什么这个区别重要

继续使用前面的例子：

```text
query:
  "不支持云同步的本地 Rust 方案"

候选 A:
  "Rust 本地存储，不需要云同步"
  → 真正相关

候选 B:
  "Rust 项目需要云同步"
  → 主题接近，但实际不相关
```

双编码器分别看到 A、B，可能因为都包含：

```text
Rust
本地
云同步
```

而产生相近的向量，难以准确区分“需要”和“不需要”。

Cross-encoder 会同时看到：

```text
query: 不支持云同步
A:     不需要云同步
B:     需要云同步
```

因此更容易判断：

```text
A 相关
B 不相关
```

##### 为什么 Cross-encoder 不能预先计算

双编码器可以提前做：

```text
所有 document → document vectors
```

查询时只需要：

```text
query vector · document vector
```

Cross-encoder 的分数依赖具体的 query-document 组合：

```text
query1 + document → score
query2 + document → score
query3 + document → score
```

文档分数不能脱离 query 提前计算，因此每个查询都必须现场组合候选并运行模型。

##### 两者和“实时训练”无关

双编码器和 Cross-encoder 在查询时通常都只是推理：

```text
双编码器：
  推理 query embedding
  推理 document embedding

Cross-encoder：
  推理 query-document relevance
```

两者都不会在每次查询时更新模型参数。

真正的区别是：

```text
双编码器：
  分开编码，比较向量
  速度快，适合大规模召回

Cross-encoder：
  联合编码，直接打相关性分
  速度慢，适合少量候选精排
```

职责分工可以总结为：

| 组件 | 输入 | 是否联合阅读 | 速度 | 职责 |
|---|---|---:|---:|---|
| 双编码器 | query 和 document 分开 | 否 | 快 | 召回候选 |
| Cross-encoder | query + document | 是 | 慢 | 精排候选 |

#### 6.8.2 Cross-encoder 做了什么

Rerank 使用 cross-encoder，把 query 和 document 一起输入模型：

```text
[query, candidate document]
        ↓
cross-encoder
        ↓
relevance score
```

与双编码器不同，query 和 document 在 Transformer 内部发生 token 级交互，因此模型可以直接判断：

- 文档是否真正回答 query
- 条件是否满足
- 否定关系是否一致
- 主题相似是否掩盖了内容不相关

可以理解为：

```text
向量 / BM25 / Hybrid：
  快速筛选“可能相关的候选人”

Cross-encoder：
  逐条检查候选人，重新决定最终排名
```

#### 6.8.3 为什么不直接 Rerank 全库

Cross-encoder 成本很高。

向量检索可以提前离线计算所有 document embedding：

```text
N 个文档
→ 预先建立 embedding
→ 查询时只做点积或近似 KNN
```

Cross-encoder 必须针对每个新的 `(query, document)` 组合现场推理：

```text
N 个候选 × 每个 query
→ N 次 Transformer 推理
```

如果全库有大量 chunk，逐对推理会造成无法接受的延迟。

因此采用两阶段结构：

```text
全库 FTS / 向量 / Hybrid
  ↓
截取固定头部候选池
  ↓
只对这些候选运行 Cross-encoder
  ↓
重新排序
```

#### 6.8.4 当前实现的 Rerank 流程

核心函数是：

`src/search/rerank.rs`

```rust
pub fn rerank_and_paginate(
    rows: Vec<SearchResult>,
    offset: usize,
    limit: usize,
    request: &RerankRequest<'_>,
) -> Result<Vec<SearchResult>>
```

执行步骤：

1. 从前一阶段结果中取固定候选池。

```rust
let pool_size = request.candidates.min(rows.len());
let pool = rows[..pool_size].to_vec();
let tail = rows[pool_size..].to_vec();
```

2. 为候选池构造 query-document 文档。

文档通常是：

```text
body + "\n" + title
```

并截断到配置上限，默认：

```text
reranker_max_document_chars = 2000
```

3. 调用 cross-encoder 打分。

```rust
let scores = request.provider.rerank(query, &documents)?;
```

4. 检查分数数量和范围。

每个分数必须：

- 数量等于候选数量
- 是有限值
- 落在 `[0, 1]`

5. 对候选池按 Rerank 分数稳定降序排列。

```rust
let mut order: Vec<usize> = (0..pool.len()).collect();
order.sort_by(|left, right| {
    scores[*right]
        .partial_cmp(&scores[*left])
        .unwrap_or(std::cmp::Ordering::Equal)
});
```

相同分数保持原召回顺序。

6. 用 Rerank 分数替换原分数。

```rust
let mut row = pool[index].clone();
row.score = scores[index];
```

7. 未进入 pool 的 tail 使用降分公式。

```text
floor / (index + 2)
```

这样 tail 不会凭借原始 cosine 或 fused 分越过已经精排的 pool。

8. 最后按 offset / limit 分页。

#### 6.8.5 Cross-encoder 的实际打分位置

模型 provider 在：

`src/runtime/rerank.rs`

```rust
impl RerankProvider for OnnxRerankProvider {
    fn rerank(&self, query: &str, documents: &[String]) -> Result<Vec<f32>> {
        let ranked = model.rerank(query, texts, false, None)?;
        ...
    }
}
```

fastembed 返回的是按模型分数排好序的结果，但 provider 会按照输入下标还原成：

```text
输入文档顺序 → 模型分数数组
```

这是因为 `rerank_and_paginate` 需要自己控制稳定排序和分页语义。

模型 logit 会通过：

```rust
squash_logit(result.score)?
```

压缩到 `[0, 1]`。

#### 6.8.6 Rerank 在哪里被调用

向量-only：

`src/search/vector.rs`

```rust
Some(request) => rerank_and_paginate(results, offset as usize, limit as usize, request)?,
```

Hybrid：

`src/search/vector.rs`

```rust
Some(request) => rerank_and_paginate(results, offset as usize, limit as usize, request)?,
```

完整顺序：

```text
向量 / Hybrid 召回
  ↓
hydrate_entries
  ↓
完整 SearchResult 列表
  ↓
rerank_and_paginate
  ↓
Rerank 后的页面
```

Rerank 不是文本-only 路径的一部分。当前它挂在需要 embedding 的语义召回路径上。

#### 6.8.7 Rerank 解决什么，不能解决什么

Rerank 能解决：

- 候选已经召回，但排序不准确
- 向量认为主题相似，但实际相关性不够
- BM25 和向量融合顺序存在偏差
- 需要 query-document 深度交互才能判断的条件

Rerank 不能解决：

- 正确文档没有进入候选池
- 上游候选窗口太小
- 向量索引过期
- 模型对当前语言或领域适配不足

因为 cross-encoder 只能看到已经进入 pool 的候选：

```text
正确文档未进入 pool
  → Rerank 看不到
  → 仍然无法返回
```

召回和精排的职责可以概括为：

```text
召回：负责“找得到”
Rerank：负责“排得准”
```

#### 6.8.8 为什么 Rerank 默认关闭

当前实现与参考实现一样，默认关闭 Rerank，原因是：

- 需要额外的 ONNX 模型和运行时。
- 模型首次加载有冷启动成本。
- 每个 query 需要额外模型推理。
- 对简单关键词查询，收益可能不明显。
- 模型有语言和领域限制。

所以它是可选的：

```text
不启用 Rerank：
  更快
  排序依赖 BM25、cosine 和固定融合公式

启用 Rerank：
  更慢
  头部候选通常更准确
```

#### 6.8.9 Rerank 与分页

启用 Rerank 时：

```rust
rerank_and_paginate(results, offset, limit, request)
```

分页行为：

- 只重排固定头部候选池。
- 默认通常取前 20 条。
- 文档文本是 `body + "\n" + title`。
- 默认截断到 2000 字符。
- cross-encoder 分数替换头部结果原分数。
- 未进入 pool 的尾部结果降分。
- 在底层候选池不变时，分页会重新计算同一个 pool，因此页间顺序稳定。

没有 Rerank 时：

```rust
results.into_iter().skip(offset).take(limit).collect()
```

需要注意：这里的“分页顺序稳定”前提是底层候选集合、索引和文档内容没有变化。
如果数据在翻页期间发生变化，仍然可能发生重复或跳过，见第 7 章风险分析。

### 6.9 返回语义

Hybrid 返回：

```text
total = 0
total_is_exact = false
has_more = 探测结果是否超过 page_size
```

`total = 0` 不代表没有结果，只表示当前实现没有计算精确总数。

调用方应使用：

- `results.len()`
- `has_more`
- `current_page`
- `page_size`

不要根据 `total = 0` 判断 Hybrid 无结果。

---

## 7. 重要边界和潜在风险

### 7.1 归一化依赖候选窗口

FTS 分数的最大值来自当前候选窗口。窗口变化时，同一个文档的归一化分数可能变化。

风险：

- 窗口内只有一个 FTS 结果时，它会直接获得 1.0。
- 窗口内其他结果变化会影响当前结果的相对分数。
- 该分数适合窗口内排序，不适合跨查询解释。

### 7.2 向量先截 chunk，再聚合到行

`aggregate_matches` 先取 top-k chunk，再按 `(type, id)` 聚合成结果行。

风险：

- 一篇长笔记的多个高相关 chunk 可能占满 top-k。
- 其他笔记的 chunk 可能因此没有进入候选。
- 增加候选窗口只能缓解，不能彻底消除。

### 7.3 向量候选受 4096 上限约束

`ranked_matches` 使用：

```text
k.min(MAX_VECTOR_K)
```

当前 `MAX_VECTOR_K = 4096`。传入更大的 chunk pool 也会被截断。

### 7.4 向量过滤使用 filter-only FTS 扫描

过滤请求会额外执行一次过滤型 `search_text`：

```text
VECTOR_FILTER_SCAN_LIMIT = 50_000
```

风险：

- 过滤集合超过 50000 行时可能漏掉候选。
- 先排序向量，再和过滤集合做交集，召回结果受向量窗口影响。

### 7.5 多标签过滤只使用第一个 tag

文本路径的 tag 过滤当前只引用：

```rust
options.tags[0]
```

因此多个 tags 不是完整的：

- tag1 AND tag2
- tag1 OR tag2

而是首标签语义。

### 7.6 `has_match` 没有覆盖所有 MATCH 来源

文本路径当前判断：

```rust
let has_match = match_query.is_some() || options.title.is_some();
```

简单 `permalink_match` 可能被转换成 FTS MatchPredicate，但 `has_match` 不会感知它。

潜在结果：

- 查询实际执行了 MATCH。
- score 仍然是 `0.0`。
- 排序走 `updated_at DESC`，而不是 BM25。

### 7.7 `content_stems` 使用字节长度截断

索引构造中使用：

```rust
if stems.len() > MAX_CONTENT_STEMS_SIZE {
    stems.truncate(MAX_CONTENT_STEMS_SIZE);
}
```

`String::truncate` 要求长度位于 UTF-8 字符边界。

风险：

- 如果 6000 字节边界落在中文等多字节字符内部，会 panic。
- 更安全的实现应按字符数截断，或向前回退到最近的 UTF-8 边界。

### 7.8 全项目文本合并的排序语义

`search_all_projects` 最终按 score 降序合并各项目结果。

但文本路径的 BM25：

```text
score ASC = 越负越相关
```

因此 text 模式的全项目合并会出现“按负 BM25 降序”的行为。当前 golden 已固定该行为，属于兼容参考实现的排序语义，不是通用的 best-first 定义。

### 7.9 向量索引是独立新鲜度

Markdown 索引更新不会自动刷新向量索引。向量需要单独执行：

```bash
auto-memory reindex --embeddings
```

风险：

- 未刷新时，文本行可能是新的，但 chunk 或向量仍是旧的。
- 模型名不匹配时，向量腿可能找不到候选。

### 7.10 文本 hydration 是页级 N+1 查询

文本结果会按 entity id 逐条查询实体信息。

结果页较大时，可以改为一次：

```sql
SELECT id, permalink, external_id
FROM entity
WHERE id IN (...)
```

### 7.11 纯过滤查询的 score 固定为零

纯过滤请求没有文本相关性，因此：

```text
score = 0.0
ORDER BY updated_at DESC
```

排序表达的是最新修改优先，不是相关性优先。

---

## 8. 源码索引

| 主题 | 源码 |
|---|---|
| MCP 检索分派 | `src/adapters/mcp/server.rs` |
| 过滤选项转换 | `src/adapters/mcp/helpers.rs` |
| 文本检索入口 | `src/search/text.rs` |
| FTS query 改写 | `src/search/query.rs` |
| relaxed OR | `src/search/relaxation.rs` |
| 索引行构造 | `src/search/index_rows.rs` |
| 语义切块 | `src/search/chunking.rs` |
| 向量与 Hybrid | `src/search/vector.rs` |
| Rerank 流程 | `src/search/rerank.rs` |
| Rerank Provider | `src/runtime/rerank.rs` |
| FTS5 和向量表结构 | `src/storage/schema.rs` |
| Store API | `src/storage/store.rs` |
| 向量化重建 | `src/indexing/service.rs` |
| CLI 检索 | `src/main.rs` |

---

## 9. 一句话总结

文本召回负责“字面命中”和精确过滤，向量召回负责“语义接近”，Hybrid 使用归一化后的 BM25 与 cosine 进行 `max + 0.3 × min` 融合，最终可选 Rerank 对固定头部候选做精排；当前实现与参考行为高度兼容，但在候选窗口归一化、top-k 截断、多标签过滤、向量新鲜度和 `content_stems` 截断方面仍有关注点。
