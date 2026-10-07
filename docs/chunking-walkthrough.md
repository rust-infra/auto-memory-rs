# 切块是怎么发生的：跟着一个例子走

> 配套阅读：[vector-pipeline-tutorial.md](vector-pipeline-tutorial.md) §2.3。
>
> 代码：`src/search/chunking.rs`。
>
> 先记住三句话：
>
> 1. heading 和 bullet 是段落边界。
> 2. 短 section 会尽量合并，直到再加一段就超过 900 字符。
> 3. 超长 paragraph 会按 900 字符开窗，相邻窗口重叠 120 字符。

## 1. 示例文档

假设 `simple.md` 最终拼出的 embedding source text 是：

```markdown
# 开场
[A：100 字正文]

## 中段
[B：600 字正文]

## 继续
[C：400 字正文]

- 第一条 bullet
- 第二条 bullet
bullet 后面的普通文字

## 长段
[D：100 字正文]

[E：1000 字正文]

- 最后一条 bullet
```

方括号中的 `A/B/C/D/E` 是长度标记，不是文件里的真实文字。

## 2. 第一遍：先切 section

函数逐行扫描。看到 heading 或 bullet 时，它先结束前一个 section，再把这一行放进新 section。

| Section | 内容 | 大约长度 |
|---|---|---:|
| S1 | `# 开场` + A | 104 |
| S2 | `## 中段` + B | 604 |
| S3 | `## 继续` + C | 404 |
| S4 | `- 第一条 bullet` | 约 20 |
| S5 | `- 第二条 bullet` + 后面的普通文字 | 约 35 |
| S6 | `## 长段` + D + 空行 + E | 约 1110 |
| S7 | `- 最后一条 bullet` | 约 20 |

请注意：

- 第一行就是 heading，所以它直接成为 S1。
- 第二个 heading 结束时，S1 已经被提交。
- 每个 bullet 也会结束前一个 section。
- S5 虽然以 bullet 开头，但后面的普通文字没有再次切成新 section。

## 3. 第二遍：合并和输出 chunk

现在按顺序处理 section：

```text
S1  普通文本
     -> current = S1

S2  普通文本
     -> S1 + 空行 + S2 <= 900
     -> current = S1 + S2

S3  普通文本
     -> current + 空行 + S3 > 900
     -> 输出 current
     -> current = S3

S4  bullet
     -> 先输出 current
     -> 再输出 S4 自己

S5  bullet
     -> current 已经为空
     -> 直接输出 S5

S6  长 section
     -> 分为 paragraph D 和 paragraph E
     -> D 先成为 current
     -> E 超过 900，先输出 current（包含 heading + D）
     -> E 再切成 E-900 和 E-220
     -> E-900 输出，E-220 暂时留在 current

S7  bullet
     -> 先输出 E-220
     -> 再输出 S7

结束
     -> current 为空，不需要再输出
```

最终得到 8 个 chunk：

| Chunk | 内容 |
|---:|---|
| 1 | S1 + 空行 + S2 |
| 2 | S3 |
| 3 | S4 |
| 4 | S5 |
| 5 | `## 长段` + D |
| 6 | E 的前 900 字符 |
| 7 | E 的最后 220 字符，其中前 120 字符与 chunk 6 重叠 |
| 8 | S7 |

## 4. 只看条件，不看代码

| 遇到什么 | 函数怎么处理 |
|---|---|
| heading | 结束前一段，heading 开始新 section |
| 短 bullet | 先结束前一段，再把整个 bullet section 独立输出为一个 chunk |
| 超过 900 字符的 bullet | 不进入 bullet 独立输出规则，先按普通长 section 做 900/120 窗口拆分 |
| 短普通段落 | 尝试和当前 chunk 合并 |
| 合并后不超过 900 | 继续累积 |
| 合并后超过 900 | 先输出旧 chunk，再开始新 chunk |
| 单个 paragraph 超过 900 | 按 900 字符切成多个窗口 |
| 窗口不是最后一块 | 下一块从当前末尾往前退回 120 字符 |
| 最后还有 current | 最后统一输出 |
| 输入为空 | 直接返回空数组 |

这里的分支有明确优先级：

```text
section > 900
  -> split_long_section()

否则 section 是 bullet
  -> flush current，bullet 独立成 chunk

否则普通 prose
  -> 尝试合并到 current_chunk
```

所以“bullet 独立成 chunk”只对不超过 900 字符的 bullet section 成立。长 bullet 会先被
字符窗口切分；如果拆分后还有后续 section，最后一个窗口仍可能和其他 section 继续交互。
具体验证见 C11。

长度计算的是 **Unicode 字符数**，不是 UTF-8 字节数。

## 5. 为什么 bullet 要独立，relation / observation 不需要

bullet 规则不是按“entity / observation / relation”做判断，而是在更晚的文本层补救。

真正的关系是：

```text
解析 Markdown
  -> observation / relation 在 search_index 中各自拥有独立 row
  -> entity row 仍然包含整篇 notes body
  -> 只有 entity body 里的 bullet 还需要在 chunking 阶段再次拆开
```

| 来源 | chunking 前已经是什么 | 送给 chunking 的文本 | 需要 bullet 规则吗 |
|---|---|---|---|
| entity | 一篇笔记一个 row；正文里可能包含多条 bullet fact | title + permalink + 完整 body | 需要，否则多个 fact 可能挤进同一个 chunk |
| observation | 一条 observation 已经是一个独立 row | 合成的 title + permalink + category + content | 不需要，row 本身已经原子化 |
| relation | 一条 relation 已经是一个独立 row | 合成的 title + permalink + relation_type | 不需要，row 本身已经原子化 |

例如 relation 在 Markdown 里可能写作：

```markdown
- links_to [[projects/alpha]]
```

但 relation row 拼给 embedding 的文本不是原始 Markdown 行，而是：

```text
simple -> Alpha Project

oracle/notes/simple/links-to/oracle/projects/alpha

links_to
```

这里没有 `- ` 这个 bullet 标记，因为 chunking 拿到的是已经解析、解析目标、合成 title 后的 relation 字段。

换成一句话：

> observation 和 relation 的“每项独立”发生在 `search_index` row 层；
> bullet 规则主要用来让 entity 正文里的列表项获得同等级别的独立性。

但这里要区分“设计目的”和“代码条件”：

- `split_text_into_chunks(text: &str)` 没有 `item_type` 参数，不知道当前是 entity、observation 还是 relation。
- `is_bullet` 只看传入文本的第一行是不是 `- ` 或 `* ` 开头。
- observation / relation 通常不会触发它，不是因为代码主动排除了它们，而是因为
  `compose_row_source_text()` 没有把原始 Markdown bullet 标记放进它们的 source text。
- 如果某个合成 title 或 content 恰好以 `- ` / `* ` 开头，通用文本逻辑仍可能把它识别成 bullet。

所以准确说法是：**bullet 规则主要服务于 entity body，但实现是通用文本规则，不是 entity 专属分支。**

参考实现 `semantic_chunking.py` 的注释也说明了这个目的：保留 bullet 的边界，
是让单条 fact 拥有自己的 retrieval vector。

## 6. 字段什么时候会独立成 chunk

`title`、`permalink`、`category`、`content` 这些都只是字段，不是 chunk 类型。
`compose_row_source_text()` 把它们用空行连接起来：

```text
title

permalink

category

content
```

真正切块时，`split_text_into_chunks()` 看不到字段名，只能看到文字和空行。

- 整段不超过 900 字符时，字段通常合并在同一个 chunk。
- 整段超过 900 字符时，`split_long_section()` 会把空行当作 paragraph 边界。
- 如果前面的标题和 permalink 很短，而下一段 content 很长，前缀字段可能先组成一个独立 chunk。
- 如果某个字段本身就超过 900 字符，它会进入字符窗口，而不是独占一个完整的 chunk。

例如 entity source text：

```text
Title

project/notes/example

1000 个 x
```

会变成：

```text
chunk 1 = "Title\n\nproject/notes/example"
chunk 2 = 900 个 x
chunk 3 = 最后 220 个 x
```

这就是“字段看起来独立占了 chunk”的原因，但实际决策单位仍然是 paragraph 和 900 字符上限。

## 7. 这个例子触碰不到的两种情况

这两个分支不能同时放进同一个单次调用。

### 7.1 空输入

```text
split_text_into_chunks("") -> []
```

空输入在 sectioning 之前就返回了，所以它不能和正常文档出现在同一次执行里。

### 7.2 最后的 current 是否非空

当前例子最后是 bullet：

```text
... E-220 -> S7 bullet
```

因此：

```text
遇到 S7 -> 输出 E-220
结束时  -> current 为空
```

如果删掉 S7，那么 E-220 会走到最后的统一输出：

```text
遇到 E-220 -> current = E-220
结束        -> 输出 current
```

两种情况在代码里只差最后一个 bullet，但走的是不同的结束分支。

## 8. 对应自动化测试

实现中的 `branch_tour_*` 测试覆盖了这篇文章提到的所有条件、字段边界和防御路径；
C09 专门验证本文这份示例文档会产生预期的 8 个 chunk；C11 验证超长 bullet 的条件优先级：

| Case | 内容 |
|---|---|
| C01 | 空输入与纯空白输入 |
| C02 | heading、bullet、合并以及空 tail |
| C03 | 普通 section 合并超过 900 后 flush |
| C04 | 长 section 前已有 current chunk |
| C05 | 单个 2500 字符 paragraph 的窗口和 overlap |
| C06 | 空 paragraph、空窗口、bullet item 拆分等 helper 分支 |
| C07 | 短 paragraph 后接超长 paragraph |
| C08 | heading / bullet 分类器的边界输入 |
| C09 | 本文的单一示例文档，验证 8 个 chunk 的形状和长度 |
| C10 | 字段前缀与长 content 分开，验证 metadata prefix 可以成为独立 chunk |
| C11 | 超长 bullet 先走长 section 规则，验证条件优先级 |

运行：

```bash
cargo test -p auto-memory-rs --lib search::chunking::tests::
```

分支覆盖率：

```bash
cargo llvm-cov --lib --branch --summary-only -- search::chunking::tests::
```

当前目标运行中，`search/chunking.rs` 的 branch coverage 为 **100%**。
