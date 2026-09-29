# 检索全流程教学（真实数据版）

> **这份文档怎么用**：用同一个查询走完三条检索通道（关键词 / 语义 / 混合），每一步都给出**真实数字**，
> 并附 fixture vault 上的完整场景清单。数据来源见文末。
>
> 配套阅读：[vector-pipeline-tutorial.md](vector-pipeline-tutorial.md)（向量是怎么算出来并落库的）。
>
> 🎬 **动画版**：交互页 [visuals/search-pipeline.html](visuals/search-pipeline.html)（7 步自动播放，可单步 / 暂停）；
> 贴到文档 / 聊天用 GIF：[visuals/search-pipeline.gif](visuals/search-pipeline.gif)（1180×1420，8.7 秒）；
> 投屏 / 上传视频平台用 MP4：[visuals/search-pipeline.mp4](visuals/search-pipeline.mp4)（H.264，8.5 秒）。
> 想一次看完向量化 + 检索 14 步：[visuals/pipeline-tutorial.mp4](visuals/pipeline-tutorial.mp4)（合并版，带章节标记）。带背景音的版本是 [visuals/pipeline-tutorial-music.mp4](visuals/pipeline-tutorial-music.mp4)。
> 要中文旁白讲解的用 [visuals/pipeline-tutorial-narrated.mp4](visuals/pipeline-tutorial-narrated.mp4)（2 分 29 秒，神经语音，每步按讲解长度展开）。

---

## 1. 三条通道，回答三个不同的问题

| 模式 | 命令 | 回答的问题 | 排序依据 |
|---|---|---|---|
| 关键词 | `search <q>` | "哪些行**含有这些词**" | `bm25()`（FTS5 内置） |
| 语义 | `search <q> --vector` | "哪些行**意思最接近**" | 余弦相似度 |
| 混合 | `search <q> --hybrid` | 两者取长补短 | `max + 0.3 × min` 融合 |
| 重排 | 加 `--reranker` | 对候选池做精排 | 交叉编码器（cross-encoder） |

---

## 2. 同一个查询，三条通道的真实结果

查询词：**`rust`**，项目 `oracle`，fixture vault。下面每个数字都是真跑出来的。

### 2.1 关键词（FTS）

```
auto-memory search rust --index DB --project oracle
```

| 分数 | 类型 | 标题 |
|---|---|---|
| **-2.198663** | entity | Frontmatter Demo |
| -1.726670 | entity | simple |
| -1.591522 | entity | Alpha Project |

⚠️ **分数是负数，而且越负越好**。这是 SQLite `bm25()` 的原样输出（内部按升序排），不是"相关度低"。
参考实现也这么透出，所以不要自作聪明取绝对值。

### 2.2 语义（vector）

```
auto-memory search rust --index DB --project oracle --vector --embedding-fixture FIX
```

| 分数 | 类型 | 标题 |
|---|---|---|
| **0.764210** | entity | Alpha Project |
| 0.720774 | entity | Frontmatter Demo |
| 0.654490 | entity | simple |
| 0.652250 | entity | Observations Demo |
| 0.617521 | entity | Beta Project |
| 0.587573 | entity | task-markers |

语义通道多捞回了 3 条关键词通道没找到的（Observations Demo、Beta Project、task-markers）——它们**没出现 "rust" 这个词**，但内容语境相关（离线、Rust 端口等）。

### 2.3 混合（hybrid）—— 融合算术可以手算

```
auto-memory search rust --index DB --project oracle --hybrid --embedding-fixture FIX
```

融合公式（`src/search/vector.rs` 的 `fuse_hybrid`）：

```
score = max(fts_norm, vector) + 0.3 × min(fts_norm, vector)
```

其中 `fts_norm` 是把 bm25 **按本页最大绝对值归一化**到 `[0,1]`：本例最大 `|bm25| = 2.198663`。

| 标题 | fts_norm | vector | 手算 | 实际输出 |
|---|---|---|---|---|
| Frontmatter Demo | 2.198663 / 2.198663 = **1.000000** | 0.720774 | `1.0 + 0.3×0.720774` = **1.216232** | `1.216232` ✅ |
| simple | 1.726670 / 2.198663 = 0.785327 | 0.654490 | `0.785327 + 0.3×0.654490` = **0.981674** | `0.981674` ✅ |
| Alpha Project | 1.591522 / 2.198663 = 0.723859 | 0.764210 | `0.764210 + 0.3×0.723859` = **0.981368** | `0.981368` ✅ |
| Observations Demo | 没命中 → 0 | 0.652250 | `0.652250 + 0` = **0.652250** | `0.652250` ✅ |
| Beta Project | 没命中 → 0 | 0.617521 | **0.617521** | `0.617521` ✅ |

三点值得记住：

1. **只命中一条通道也能上榜**——观测数据里 Observations Demo / Beta Project 完全没被关键词命中，靠语义进来。
2. **两条通道都命中会额外加分**：Frontmatter Demo 只比 simple 的语义分高 0.066，但融合后领先 0.235——因为它的 FTS 归一化分是满分。
3. **`max + bonus×min` 不是加权平均**：单通道强项不会被另一条通道的弱项拖下水。

### 2.4 重排（rerank）

```
auto-memory search rust --index DB --project oracle --vector --reranker --reranker-fixture FIX
```

参考实现捕获的结果（`tests/golden/search/rerank-vector-rust.json`）：

| 重排前（vector） | | 重排后（rerank） | |
|---|---|---|---|
| Alpha Project | 0.764210 | **simple** | 0.589133 |
| Frontmatter Demo | 0.720774 | Alpha Project | 0.513636 |
| simple | 0.654490 | Frontmatter Demo | 0.489848 |
| Observations Demo | 0.652250 | Observations Demo | 0.213373 |
| Beta Project | 0.617521 | 中文测试文档 | 0.211405 |

**重排会改变顺序**（simple 从第 3 升到第 1），分数尺度也完全不同（交叉编码器的输出，不是余弦）。
所以：**重排后的分数不能跟向量分数互相比较**。

---

### 2.5 换个查询看：`note`（动画用的就是它）

上面用 `rust` 讲机理解耦；动画里换成 **`note`**，因为"查询词正好写在那一行里"这件事更好演示。
下面全是**真模型**跑出来的（ONNX Runtime 1.29.0 + bge-small-en-v1.5）：

| | 第 1 | 第 2 | 第 3 | 第 4 | 第 5 |
|---|---|---|---|---|---|
| 关键词通道（bm25） | Deep Note −1.333472 | empty −1.045450 | **simple −0.994168** | Unresolved Links −0.957196 | Wikilinks Demo −0.947650 |
| 语义通道（余弦） | Deep Note 0.715897 | Same Title 0.692292 | task-markers 0.682423 | **simple 0.681014** | 中文测试文档 0.676070 |
| 混合（融合后） | Deep Note 1.214769 | empty 0.983629 | **simple 0.949853** | 中文测试文档 0.900950 | Wikilinks Demo 0.900507 |

**simple 为什么会被找到？** 看它自己的分块（`entity` 行只有两块）：

| 分块 | 相似度 | 内容 |
|---|---|---|
| `entity:9:1` ← **entity 行的分数来自它** | **+0.681014** | `- [note] Created as a baseline fixture` ← 查询词就写在这一块里 |
| `entity:9:0` | +0.671652 | `simple … # Simple Note … A plain note without frontmatter …` |

融合算术（simple）：

```
fts_norm = |−0.994168| / |−1.333472| = 0.745548
score    = max(0.745548, 0.681014) + 0.3 × min(0.745548, 0.681014)
         = 0.745548 + 0.204304 = 0.949853      ← 与实测输出一致
```

> ⚠️ **一个容易误读的地方**：`simple` 的 observation 行里那块 `observation:11:0` 相似度更高
> （**+0.725384**），但它属于**另一条搜索结果**（observation 行，id 11）。默认只搜 `entity` 行，
> 所以它不出现在结果里；显式加 `--entity-type observation` 就能看到它以 0.725384 返回（实测吻合）。
> **别把"某个块分高"直接当成"这一行分高"。**

## 3. 三条通道各自的机制

### 3.1 关键词通道（FTS5）

- 索引在 `search_index`（FTS5 虚拟表），可搜索列是 `title` / `content_stems` / `content_snippet` / `permalink`，其余列 `UNINDEXED`（存出来给结果用）。
- 分词器 `unicode61 tokenchars 0x2F`：`/` 算词字符，所以 `notes/simple` 是一个词。
- 前缀索引 `1,2,3,4`：`arch*` 这种前缀查询才快。
- 排序 `ORDER BY bm25(search_index)`。

### 3.2 语义通道（vector）

- **块级别打分，行级别聚合**：向量是按 chunk 存的，搜索时先算块相似度，再把同一行的块聚合成一个分数（`aggregate_matches`）。
- 候选上限：默认取 `DEFAULT_VECTOR_K = 100` 个块（上限 `MAX_VECTOR_K = 4096`）。
- `matched_chunk` 的填充规则：**短笔记（正文 ≤ 2000 字符）返回整篇正文，长笔记返回最多 5 个最佳块**（用 `---` 连接）。
- `--min-similarity` 可以砍掉低于阈值的行。
- **`total` 字段恒为 0**：向量通道不统计总数，翻页要看 `has_more`。

### 3.3 混合通道

见 §2.3 的算术。补充两点：

- FTS 分数先归一化（按本页最大绝对值），所以**同一批结果的相对分数才有意义**，跨查询不可比。
- 只有 FTS 命中的行，`matched_chunk` 会回退成 `content` 预览（`fallback_to_content`）。

---

## 4. 场景全集（fixture vault 上的 32 个捕获用例）

全部来自 `tests/golden/search/*.json`，命令行取自 `tests/golden/manifest.json`。

| 场景 | 命令要点 | 结果数 | 榜首 |
|---|---|---|---|
| 基础关键词 | `search rust` | 3 | Frontmatter Demo |
| 大小写不敏感 | `search RUST` | 3 | Frontmatter Demo |
| 短语查询 | `search '"source of truth"'` | 1 | Frontmatter Demo |
| 布尔查询 | `search 'rust AND architecture'` | 1 | Frontmatter Demo |
| 前缀查询 | `search 'arch*'` | 2 | Beta Project |
| 中文 | `search 测试` | 1 | 中文测试文档 |
| 无结果（**陷阱，见 FAQ**） | `search zzzz-not-present` | **15** | Deep Note |
| 标题过滤 | `--title Alpha` | 1 | Alpha Project |
| permalink 过滤（**陷阱，见 FAQ**） | `--permalink 'projects/*'` | 0 | – |
| 标签过滤 | `--tag rust` | 1 | Frontmatter Demo |
| 类型过滤 | `--type project` | 2 | Beta Project |
| 类别过滤（**隐式收窄到 observation**） | `--category decision` | 1 | decision: Alpha uses… |
| 行类型过滤 | `--entity-type observation --category decision` | 4 | decision: Keep Markdown… |
| 行类型过滤 | `search alpha --entity-type relation` | 6 | 中文测试文档 -> Alpha Project |
| 状态过滤 | `--status archived` | 1 | Beta Project |
| 元数据过滤 | `--meta status=active` | 2 | Frontmatter Demo |
| 时间过滤 | `search rust --after_date 2026-09-01` | 2 | simple |
| 时间过滤（未来） | `--after_date 2030-01-01` | 0 | – |
| 分页 | `search note --page 2 --page-size 2` | 2（共 15） | simple |
| 语义 | `--vector` + 查询 `local index` | 10 | 中文测试文档 |
| 语义 + 过滤器 | `--vector` + `--type note` | 10 | simple |
| 语义 + 行类型 | `search rust --vector --entity-type observation` | 3 | decision: Alpha uses… |
| 混合 | `--hybrid` + `rust` | 10 | Frontmatter Demo |
| 混合 + 类别 | `--hybrid --category decision` | 1 | decision: Alpha uses… |
| 混合 + 类型 | `--hybrid --type project` | 2 | Alpha Project |
| 重排 | `--vector --reranker`（3 个用例） | 10 | simple |
| 重排（混合） | `--hybrid --reranker` | 10 | simple |

**过滤器组合的原则**：`--category` 存在且没写 `--entity-type` 时，默认收窄为 `observation` 行
（`search::default_entity_types`）；否则默认只搜 `entity` 行。

---

## 5. 常见误解 FAQ

**Q：为什么 bm25 分数是负的？**
A：SQLite `bm25()` 就返回负数，越负越相关；排序按升序。参考实现原样透出。

**Q：为什么向量/混合结果的 `total` 是 0？**
A：向量通道不做总数统计。只看 `results` 和 `has_more`，别用 `total` 判断"没有结果"。

**Q：搜一个不存在的词，怎么反而返回了一整页？**
A：如果查询里含 `-` 或 `not`，FTS5 会把它当布尔算子。实测：

```
search zzzz            -> 0 条
search zzzz-not-present -> 15 条（全部！）   ← 陷阱
search zzzz not present -> 15 条（全部！）   ← 陷阱
search zzzz-notaa-presentt -> 0 条（没有 not 这个词就正常）
```

这个行为和参考实现一致（golden 里就是这么捕获的），但对用户很反直觉。**想搜带 `-` 的词就加引号**。

**Q：为什么 `--permalink 'projects/*'` 一条都搜不到？**
A：因为生成的 permalink 带**项目前缀**——实际值是 `oracle/projects/alpha`。实测：

```
--permalink 'projects/*'        -> 0 条
--permalink 'oracle/projects/*' -> 2 条（Alpha Project、Beta Project）
```

显式写在 frontmatter 里的 permalink（如 `notes/frontmatter-note`）**没有**前缀，所以那一类要按原样匹配。

**Q：为什么刚写的笔记搜不到（关键词能搜到、语义搜不到）？**
A：向量要另跑 `reindex --embeddings`。见 [vector-pipeline-tutorial.md](vector-pipeline-tutorial.md) §1.2。

**Q：混合检索会不会把关键词结果挤掉？**
A：不会。融合是 `max + 0.3×min`，单通道命中的行至少拿到自己那条通道的分数，只是没有额外加成。

**Q：重排能只用来排序、不改分数吗？**
A：不能，重排分数是另一套尺度（交叉编码器），跟向量/融合分不可比。

---

## 6. 复现

```bash
DB=/tmp/demo.db; V=tests/fixtures/vault
FIX=tests/golden/vector/embeddings-reference.json

auto-memory reindex --full --vault $V --index $DB --project oracle
auto-memory reindex --embeddings --vault $V --index $DB --project oracle --embedding-fixture $FIX

auto-memory search rust --index $DB --project oracle                     # FTS
auto-memory search rust --index $DB --project oracle --vector --embedding-fixture $FIX
auto-memory search rust --index $DB --project oracle --hybrid --embedding-fixture $FIX
```

> `--embedding-fixture` 会把 golden 里捕获的真实模型输出当"模型"用，所以离线也能复现出同样的分数。

§2.5 的 `note` 数字用的是**真模型**（ONNX Runtime **1.29.0** + 本地 bge-small 缓存），
命令与上面相同，只是不加 `--embedding-fixture`：

```bash
auto-memory search note --index $DB --project oracle --vector \
    --onnx-runtime /path/to/libonnxruntime.1.29.0.dylib --page-size 7
```

> 版本要对齐：绑定编译到 ORT API 24，仓库统一用 1.29.0（参考捕获也是这版）。用 1.30 的
> Homebrew bottle 会在建 session 时失败。

---

## 7. 数据从哪来

| 数据 | 文件 |
|---|---|
| 32 个检索用例（含 argv 与结果） | `tests/golden/search/*.json`、`tests/golden/manifest.json` |
| 查询向量（`rust`、`local index`） | `tests/golden/vector/embeddings-reference.json` |
| 融合公式与常量 | `src/search/vector.rs`（`FUSION_BONUS = 0.3`、`FTS_GATE_THRESHOLD`、`DEFAULT_VECTOR_K = 100`） |
| FTS 查询构造与松弛重试 | `src/search/query.rs`、`src/search/relaxation.rs` |

## 8. 相关文档

| 想知道 | 看这里 |
|---|---|
| 向量怎么算出来、怎么落库 | [vector-pipeline-tutorial.md](vector-pipeline-tutorial.md) |
| 检索行为契约（参数、分页、过滤器） | [search-spec.md](../specs/search-spec.md) |
| 术语（bm25 / matched_chunk / entity_types …） | [glossary.md](glossary.md) |
