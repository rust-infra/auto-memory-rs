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

### 6.7 Hydration

融合结果只有：

```text
(type, id, fused_score)
```

需要 hydration 补齐：

- title
- permalink
- file_path
- metadata
- category
- relation_type
- owning entity
- matched chunk

Hybrid 的 `HydrationEntry.fallback_to_content = true`，允许 FTS-only 行在没有向量 chunk 时使用 content snippet 回退。

### 6.8 Rerank 和分页

启用 Rerank 时：

```rust
rerank_and_paginate(results, offset, limit, request)
```

Rerank 的行为：

- 只重排固定头部候选池。
- 默认通常取前 20 条。
- 文档文本是 `body + "\n" + title`。
- 默认截断到 2000 字符。
- cross-encoder 分数会替换头部结果的原分数。
- 未进入 pool 的尾部结果会降分。
- 分页时重新对同一 pool 计算，保证页间顺序稳定。

没有 Rerank 时：

```rust
results.into_iter().skip(offset).take(limit).collect()
```

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
