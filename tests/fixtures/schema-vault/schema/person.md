---
title: Person
type: schema
entity: person
version: 1
schema:
  name: string, full name
  role?: string, job title
  email?: string, contact address
  works_at?: Organization, employer
  tags?: string
  status?(enum): [active, inactive]
settings:
  validation: warn
---

# Person Schema

Defines the shape of `person` notes for the schema fixtures.

