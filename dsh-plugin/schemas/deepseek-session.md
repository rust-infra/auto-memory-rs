---
title: DeepSeek Harness Session
type: schema
entity: DeepseekSession
version: 1
schema:
  summary?: string, one-paragraph what happened in this DeepSeek Harness thread
  changed_file?(array): string, files created, edited, deleted, or inspected
  verification?(array): string, checks run and their result
  decision?(array): string, decisions surfaced or created during the thread
  blocker?(array): string, unresolved blockers or failed approaches
  next_step?(array): string, explicit cursor for the next DeepSeek Harness thread
  produced?(array): Entity, notes or artifacts created or updated
settings:
  validation: warn
  frontmatter:
    project: string, the Auto Memory project this session belongs to
    started: string, when the session began or checkpoint was created
    ended?: string, when the session was checkpointed
    status?(enum, lifecycle of the checkpoint): [open, resumed, closed]
    cwd?: string, working directory for the DeepSeek Harness thread
    username?: string, operating-system user that created the checkpoint
    hostname?: string, host that created the checkpoint
    session_id?: string, exact host-provided session identifier (pair with agent)
    agent?: string, agent harness identifier (deepseek for this host)
    codex_session_id?: string, legacy Codex session identifier (read compatibility only)
    turn_id?: string, DeepSeek Harness turn identifier
    trigger?: string, compaction trigger or deliberate checkpoint source
    model?: string, active DeepSeek Harness model slug when known
    capture?(enum, how this checkpoint was produced): [extractive, deliberate, summarized]
---

# DeepSeek Harness Session

A **DeepseekSession** note is a resumable general checkpoint. It captures the
thread cursor: what changed, what was verified, what decisions matter, and what
the next DeepSeek Harness thread should do first.

DeepSeek Harness sessions are found by structured recall:
`search_notes(metadata_filters={"type": "deepseek_session"}, after_date="7d")`.

New checkpoints from one DeepSeek Harness chat share `agent: deepseek` and `session_id`.
Legacy `codex_session_id` remains accepted for existing notes. Each new
immutable checkpoint can use `continues [[Previous checkpoint title]]` to form a
navigable lineage without rewriting its predecessor.

## What Goes In A DeepseekSession

- **summary** - what happened.
- **changed_file** - changed or inspected paths that matter to resume.
- **verification** - commands actually run and their outcome.
- **decision** - choices made or surfaced.
- **blocker** - open failures, constraints, or rejected approaches.
- **next_step** - the next concrete action.

Validation is `warn` so checkpointing never blocks the user's flow.
