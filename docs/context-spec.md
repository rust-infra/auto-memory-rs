# Context (`build_context`) — Compatibility Contract

Reference: `basic_memory` 0.23.2 (`docs/reference.md`). **[V]** verified in source, **[G]** golden capture pending.
**[C]** verified against a captured golden (Phase 9b).

## 1. memory:// URLs

`MemoryUrl` normalization & validation (`schemas/memory.py`): `[V]`

- Accepts `memory://specs/search` or bare `specs/search`; normalized to `memory://…`.
- Reject: empty/whitespace, `://` inside path, `//` (double slash), chars `< > " | ?`.
- Max length 2028.
- Pattern matching uses `*` (not rejected).

Path segments resolve against project + entity permalinks/file paths. Project may be detected
from the memory URL prefix (e.g. `memory://<project-permalink>/<entity-path>`) or supplied via
`project`/`project_id`. `[V] mcp/project_context*.py`; resolution details `[G]`.

## 2. Tool parameters

`build_context(url, depth=1, timeframe=None, page=1, page_size=10, max_related=10,
project=None, project_id=None, output_format="json")` `[V] schemas/memory.py + mcp/tools/build_context.py`

Validation errors (tool raises): `page >= 1`; `page_size in [1,50]`; `max_related in [0,100]`;
non-integer depth rejected. `[V] mcp/tools/build_context.py`

Constants: `DEFAULT_CONTEXT_PAGE_SIZE=10`, `MAX_CONTEXT_PAGE_SIZE=50`,
`DEFAULT_CONTEXT_RELATED_RESULTS=10`, `MAX_CONTEXT_RELATED_RESULTS=100`. `[V]`

## 3. Algorithm

`services/context_service.py` `[V] [C]`:

1. Resolve the primary item(s): `search(permalink=<normalized path>, limit=page_size+1,
   offset=(page-1)*page_size)`, falling back to the link resolver (`use_search=True,
   strict=False`) when the direct lookup is empty. The resolved entity permalink becomes
   `metadata.uri`; an unresolved URL yields an **empty result set**, not an error, and any
   page past the first for a direct lookup is empty.
2. `find_related(type_id_pairs, max_depth=depth, since=timeframe, max_results=max_related)`
   is one recursive-CTE query (`_build_sqlite_query`) `[V] [C]`:
   - base case = seed entities with `created_at >= since` and `project_id = <project>`;
   - per traversal step it emits the **relation rows** touching the current entity (joined to
     their source entity, filtered by `e_from.created_at >= since` and
     `e_from.project_id = r.project_id`) and the **entities** those relations connect to
     (`created_at >= since`, cycle-checked with the accumulated `entity_path`);
   - relations are depth `n+1` and entities depth `n+2`, so `max_depth = depth * 2`;
   - the outer query keeps `MIN(depth)` per distinct row and returns
     `ORDER BY depth, type, id LIMIT max_related`. Because `entity` < `relation`
     lexicographically, an unfiltered entity row would sort before relations at the same
     depth — in practice relations occupy the odd depths, so relations precede entities.
3. Observations are loaded for the **distinct** entity ids among the primary and the related
   entity rows (`ObservationRepository.find_by_entities` groups by entity id).
4. Metadata: `total_results = primary_count + related_count`; `total_relations` counts related
   relation rows; `total_observations` sums the observation lists for the distinct entity ids
   from step 3; `timeframe` echoes `since.isoformat()`.

### 3.1 Timeframe (`since`) `[V] [C]`

- The CLI (`tool build-context`) and the MCP tool both default `timeframe="7d"`.
  `validate_timeframe` normalizes human expressions to `<days>d` (or keeps `today`);
  `parse_timeframe` resolves them with a **minimum one-day lookback**.
- `since` is passed to SQLite as `datetime.isoformat()` and compared **as a string** against
  the stored `%Y-%m-%d %H:%M:%S.%f` timestamps, so the filter behaves lexicographically.
- The filter applies to the seed entity, the relation *source* entity, the connected entity,
  and (via `relation_date`) the entity hop. It is the reason a note whose frontmatter
  `created:` predates the window disappears from `related_results` (only the relation row
  that points at it survives).

### 3.2 Stored timestamps `[V] [C]`

`entity.created_at` / `updated_at` come from `EntityMarkdown.created` / `.modified`: the
frontmatter `created`/`modified` values when present (date-only and naive values are local
wall-clock time), otherwise the file's `st_ctime` / `st_mtime`. The same values are copied to
every `search_index` row owned by that entity (`created_at=entity.created_at`).

### 3.3 What is *not* reproducible

The reference assigns entity/relation ids while indexing files **concurrently**, so two oracle
runs of the same vault produce different id orders (observed: `dup-a, deep-note, relations,
cjk, …` vs `dup-a, observations, frontmatter, simple, …`). Because the traversal orders by id
and truncates with `LIMIT max_related`, the *tail* of `related_results` — and therefore
`total_relations` / `total_observations` for truncated cases, and the exact text order — is
run-dependent. `basic-mem-rs` reproduces the rules (and the relative order of relations that
share a source, see `data-format.md`) deterministically; the golden tests compare the related
*set* (plus the untruncated rows exactly) and `tests/golden/context/find-related.json` replays
the reference traversal against the reference ids row-for-row.

## 4. Output shapes

JSON = `GraphContext` model dump `[V] schemas/memory.py`:

```json
{
  "results": [ ContextResult ],
  "metadata": {
    "uri": null,
    "types": ["entity","observation","relation"],
    "depth": 1,
    "timeframe": null,
    "generated_at": null,
    "primary_count": 0,
    "related_count": 0,
    "total_results": 0,
    "total_relations": 0,
    "total_observations": 0
  },
  "page": 1,
  "page_size": 10,
  "has_more": false
}
```

`ContextResult`: `primary_result` (entity/observation/relation summary), `observations`[],
`related_results`[].

Summary shapes: `EntitySummary` (type="entity", external_id, entity_id, permalink, title,
content, file_path, created_at), `RelationSummary` (type="relation", relation_id, entity_id,
title, file_path, permalink, relation_type, from_entity*, to_entity*, to_name, created_at),
`ObservationSummary` (type="observation", observation_id, entity_id, entity_external_id,
title, file_path, permalink, category, content, created_at).

`output_format="text"` renders a markdown summary (entity blocks + related links). Exact text
shape is captured in `tests/golden/context/<case>.md` (the CLI always asks for JSON and renders
its own plain outline, so the harness replays the MCP formatter). `[C]`

The CLI's `--plain` outline is captured in `tests/golden/context/*.txt`
(`_plain_build_context`: `Context: <uri>`, `<type>  <title>`, two-space indented body,
`[category] content` observations truncated at 120 chars, then
`<relation_type>  <type>  <title>` related lines). `[C]`

## 5. recent_activity (related surface)

`recent_activity(project?, timeframe, page, page_size, type?, output_format?)` returns either
per-project activity or discovery across projects; JSON uses `ProjectActivitySummary`
(`projects: {name: ProjectActivity}`, `summary: ActivityStats`, `timeframe`,
`generated_at`, `guidance`) with each project's `activity: GraphContext`. `[V] schemas/memory.py`
Exact defaults and text output `[G]`.

## 6. Rust compatibility rules

1. Same URL validation/normalization rules.
2. Same defaults and hard bounds (depth/page_size/max_related) and same validation errors.
3. Same traversal semantics: relation edges both directions; depth scaling ×2; dedupe by
   (type,id); max_related cap.
4. Same JSON key names/types; same text vs json dispatch.
5. Golden fixtures: 1-hop/2-hop/3-hop, cycles, duplicate titles, unresolved links, empty graph,
   page/max_related bounds, project-prefixed URLs, cross-project isolation.
