# Schema（Picoschema）教学（真实数据版）

> **这份文档怎么用**：讲清"用一篇笔记描述另一类笔记的形状"这件事——怎么写 schema、
> 怎么校验、怎么从现有笔记反推、怎么发现漂移。所有输出都是真跑出来的。
>
> 配套阅读：[data-format.md](data-format.md)（frontmatter 契约）、[glossary.md](glossary.md)（术语）。

---

## 1. 基本概念

**schema 就是一篇普通笔记**，只是 `type: schema`，并用 frontmatter 描述另一类笔记应该长什么样：

```yaml
# tests/fixtures/schema-vault/schema/person.md
---
title: Person
type: schema          # ← 它是 schema 而不是普通笔记
entity: person        # ← 它描述哪一类笔记（按 type 匹配）
version: 1
schema:
  name: string, full name
  role?: string, job title
  email?: string, contact address
  works_at?: Organization, employer
  tags?: string
  status?(enum): [active, inactive]
settings:
  validation: warn    # ← warn（默认）或 strict
---
```

四个要点：

1. **schema 也是笔记**：它自己会被索引、能被搜索、能出现在图里，只是额外承担"描述别人"的职责。
2. **`entity:` 决定它管谁**：值为 `person` 就管所有 `type: person` 的笔记（大小写会归一化，见 FAQ）。
3. **字段名对应 observation 的 category，或 relation 的 type**：`name` 期望一条 `- [name] ...`；
   `works_at` 期望一条 `- works_at [[...]]`。
4. **`settings.validation`**：`warn` 模式下问题只算警告（`passed` 仍是 true）；`strict` 模式下算错误（`passed` 变 false）。

---

## 2. Picoschema 语法

`tests/golden/schema/picoschema.json` 的 `parse` 段落里覆盖了 9 种形态：

| 写法 | 含义 |
|---|---|
| `name: string` | 必填字符串 |
| `name: string, full name` | 逗号后面是**描述** |
| `role?: string` | `?` = 可选 |
| `tags?(array): string` | `(array)` = 数组 |
| `status?(enum): [active, inactive]` | 枚举（YAML 列表形式） |
| `status?(enum): "[active, inactive], 说明"` | 枚举（字符串形式，逗号后接描述） |
| `metadata?(object): {source: string}` | 嵌套对象 |
| `plain-object: {label: string}` | 不加 `(object)` 也能是对象 |
| `works_at?: Organization` | **首字母大写 = 实体引用**（指向某种 type） |

还有几条边角规则（都在 golden 里）：

- 描述里可以包含**逗号、括号、方括号**（`parentheses-in-names-and-descriptions` 用例）。
- 空 schema（`empty-schema`）是合法的。
- 枚举只有一个值也算枚举（`enum-single-value`）。

---

## 3. 三个工具

| 工具 | 命令 | 回答的问题 |
|---|---|---|
| 校验 | `schema validate [type\|path]` | 现有笔记符合 schema 吗？ |
| 反推 | `schema infer <type>` | 从现有笔记**能推出**什么 schema？ |
| 漂移 | `schema diff <type>` | schema 和实际用法偏到哪去了？ |

找 schema 的顺序（`resolve` 的 5 个用例）：
**笔记内联 → 显式引用 → 按 `type` 隐式匹配 → 没有 schema → 没有 type**。

---

## 4. 校验：真实输出

### 4.1 `schema validate person`

```bash
auto-memory schema validate person --index DB --project schema --vault tests/fixtures/schema-vault
```

```json
{"note_type": "person", "total_notes": 2, "total_entities": 2,
 "valid_count": 2, "warning_count": 1, "error_count": 0}
```

逐篇看：

| 笔记 | passed | 问题 |
|---|---|---|
| Ada Lovelace | ✅ | 只有 `email` 缺失（可选字段） |
| Grace Hopper | ✅（warn 模式） | `status = retired` **不在枚举里**；另有 role/email/works_at/tags 缺失 |

Grace 的错误行长这样：

```json
{"field_name": "status", "status": "enum_mismatch",
 "message": "Field 'status' has invalid value(s): retired (allowed: active, inactive)"}
```

注意 schema 里 `status?(enum): [active, inactive]`，而笔记里写的是 `retired`。

### 4.2 `schema validate project`（strict 模式）

`schema/project.md` 里写的是 `settings.validation: strict`，同一份笔记的结果就变了：

```json
{"note_type": "project", "total_notes": 1, "valid_count": 0, "warning_count": 0, "error_count": 1}
```

| 笔记 | passed | 问题 |
|---|---|---|
| Alpha Project | ❌ | `status = paused` 不在 `[active, archived]` 里 → **error** |

**同一个 `enum_mismatch`，warn 下是警告、strict 下是错误**——这是 `passed` 唯一的决定因素。

### 4.3 三种字段状态

| status | 含义 |
|---|---|
| `present` | 找到了（observation 或 relation） |
| `missing` | 没找到（必填字段会带提示信息） |
| `enum_mismatch` | 找到了，但值不在枚举里 |

另外还有"**没匹配上任何字段**"的内容，它**不会**让校验失败，只被单独列出：

```json
{"unmatched_observations": {"hobby": 2}, "unmatched_relations": ["mentor"], "passed": true}
```

### 4.4 没有 schema 的类型

```bash
auto-memory schema validate meeting
→ {"error": "No schema found for type 'meeting'"}
```

---

## 5. 反推：`schema infer person`

```bash
auto-memory schema infer person --index DB --project schema --vault tests/fixtures/schema-vault
```

真实输出（`notes_analyzed: 2`）：

| 字段 | 来源 | 出现 | 百分比 | 数组？ | 目标类型 | 样例 |
|---|---|---|---|---|---|---|
| `name` | observation | 2/2 | 1.00 | 否 | – | Ada Lovelace, Grace Hopper |
| `status` | observation | 2/2 | 1.00 | 否 | – | active, retired |
| `role` | observation | 1/2 | 0.50 | 否 | – | Mathematician |
| `tags` | observation | 1/2 | 0.50 | **是** | – | pioneer, history |
| `hobby` | observation | 1/2 | 0.50 | 否 | – | sailing |
| `works_at` | **relation** | 1/2 | 0.50 | 否 | organization | organizations/analytical-engine |

两条规则要记住：

1. **计数按"笔记"不按"条数"**：Ada 有两条 `tags`，但只算 **1 篇笔记**（presence，不是 occurrences）。
   下面 `tags` 依然被推断成数组，靠的是第 2 条规则。
2. **数组推断**：`多次出现的笔记数 > 含该字段的笔记数 / 2`。
   本例 `tags`：含它的笔记 1 篇，其中 1 篇出现多次 → `1 > 0.5` → **数组**。

`works_at` 那一行值得注意：它来自 **relation**（不是 observation），并且带出了目标类型 `organization`
——因为目标笔记的 `type` 是 organization。

---

## 6. 漂移：`schema diff person`

```bash
auto-memory schema diff person --index DB --project schema --vault tests/fixtures/schema-vault
```

```json
{
  "note_type": "person",
  "schema_found": true,
  "new_fields":      [ {"name": "hobby", "source": "observation", "count": 1, "total": 2, "percentage": 0.5} ],
  "dropped_fields":  [ {"name": "email", "source": "observation", "count": 0, "total": 2, "percentage": 0.0} ],
  "cardinality_changes": [ "tags: schema declares single-value but usage is typically array" ]
}
```

三类的含义：

| 类别 | 例子 | 该做什么 |
|---|---|---|
| `new_fields` | `hobby`（笔记里用了，schema 没写） | 决定是补进 schema 还是删掉那两行 |
| `dropped_fields` | `email`（schema 写了，没人用） | 考虑改成可选或删掉 |
| `cardinality_changes` | `tags` 声明单值、实际是数组 | 把 schema 改成 `tags?(array): string` |

`cardinality_changes` 这一条正是 §5 的数组推断规则在起作用——**infer 和 diff 用的是同一套统计**。

---

## 7. 场景全集（38 个捕获用例）

`tests/golden/schema/picoschema.json` 按模块拆开：

| 模块 | 用例数 | 覆盖的场景 |
|---|---|---|
| `parse` | 9 | 标量/可选、数组、实体引用、枚举三种写法、嵌套对象、名字或描述里带括号、空 schema |
| `parse_schema_note` | 5 | warn/strict、错误别名、默认值、frontmatter 设置 |
| `parse_schema_note_errors` | 4 | 缺 entity、缺 schema、schema 不是字典、validation 非法 |
| `validate` | 7 | 全通过、缺必填、枚举不符、未匹配内容、strict 升级、frontmatter 缺字段与不符、frontmatter 数组值 |
| `infer` | 4 | 空、均匀、混合频次、关系多目标 |
| `diff` | 4 | 无漂移、新增与删除、基数变化、空 |
| `resolve` | 5 | 内联、显式引用、按 type 隐式、无 schema、无 type |

另有 30 个 MCP 帧 + 14 条 CLI 运行，捕获在 `tests/golden/mcp/schema.json`（三个 vault：有 schema / 无 schema / schema 损坏）。

---

## 8. 常见误解 FAQ

**Q：schema 笔记本身会被索引吗？**
A：会。它是一篇普通笔记（`type: schema`），照样进索引、进搜索、进向量库。

**Q：`type: Person` 和 `type: person` 算两种类型吗？**
A：**比较时不算**。入库保存原样（`Person` 就是 `Person`），但比较时经 `normalize_note_type` 归一化成
`person`——所以 Grace Hopper（`type: Person`）照样被 `person` 的 schema 管住。

**Q：字段名必须跟 observation 的类别一模一样吗？**
A：是。`name: string` 就要求有 `- [name] ...`。关系字段则匹配 relation 的 type（`- works_at [[...]]`）。

**Q：多出来的内容会让校验失败吗？**
A：不会。未匹配的 observation/relation 只记在 `unmatched_observations` / `unmatched_relations` 里，
`passed` 不受影响。

**Q：为什么 warn 模式下有错误还是 passed？**
A：`settings.validation` 决定严重度：warn → 问题只算警告；strict → 升级为错误。见 §4.1 / §4.2 对比。

**Q：`--strict` 参数和 schema 里的 `settings.validation: strict` 是一回事吗？**
A：不是。`--strict` 是**命令行的退出码开关**（有问题就返回非零），schema 里的 strict 决定**报告里算警告还是错误**。

**Q：infer 出来的数组判断看不懂。**
A：只记一条：**"含这个字段的笔记里，超过一半出现多次"才算数组**，和总笔记数无关。

---

## 9. 复现

```bash
DB=/tmp/schema.db; V=tests/fixtures/schema-vault

auto-memory reindex --full --vault $V --index $DB --project schema

auto-memory schema validate person  --index $DB --project schema --vault $V
auto-memory schema validate project --index $DB --project schema --vault $V   # strict
auto-memory schema validate meeting --index $DB --project schema --vault $V   # 无 schema
auto-memory schema infer person     --index $DB --project schema --vault $V
auto-memory schema diff person      --index $DB --project schema --vault $V
auto-memory schema validate person  --index $DB --project schema --vault $V --text   # 人读格式
```

---

## 10. 数据从哪来

| 数据 | 文件 |
|---|---|
| schema 与笔记样例 | `tests/fixtures/schema-vault/` |
| 38 个纯函数用例 | `tests/golden/schema/picoschema.json` |
| MCP / CLI 帧 | `tests/golden/mcp/schema.json` |
| 本文的 validate / infer / diff 输出 | 在上面的 vault 上实跑所得 |
| 实现 | `src/schema/`（parser / resolver / validator / inference / diff）、`src/application/schema*.rs` |

## 11. 相关文档

| 想知道 | 看这里 |
|---|---|
| frontmatter / observation / relation 契约 | [data-format.md](data-format.md) |
| 术语（note_type / observation category / relation type） | [glossary.md](glossary.md) |
| 向量化全流程 | [vector-pipeline-tutorial.md](vector-pipeline-tutorial.md) |
| 检索三条通道 | [search-pipeline-tutorial.md](search-pipeline-tutorial.md) |
