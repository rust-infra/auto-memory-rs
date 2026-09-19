---
title: Project Schema
type: schema
entity: project
version: 1
schema:
  status?(enum): [active, archived]
  owner?: string, accountable person
settings:
  validation: strict
---

# Project Schema

Strict-mode schema, so enum mismatches are errors rather than warnings.

