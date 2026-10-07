# Licensing, derivation, and what can be sold

- **Status:** Engineering analysis. Not legal advice — a lawyer has to sign off before any
  commercial commitment, and this document is the input to that review.
- **Date:** 2026-09-19
- **Resolves:** open decision #12 in `specs/compatibility-spec.md` §7 ("AGPL-3.0 obligations and
  `auto-memory-rs` naming (needs legal review)").

## 1. What this repository is licensed under

`Cargo.toml` declares `license = "AGPL-3.0-or-later"`, the README says the same, and `LICENSE`
now carries the unmodified GNU Affero General Public License version 3 text (661 lines, taken
verbatim from the reference distribution's own copy so the two cannot drift).

There is no `LICENSE` exception, no contributor agreement, and no CLA. Every contribution so
far is under AGPL-3.0-or-later like the rest.

## 2. This is a derivative work, and the AGPL's network clause therefore applies

`auto-memory-rs` is not an independent implementation that happens to behave similarly. It is
deliberately, documentedly a port of **Basic Memory 0.23.2**:

| Evidence | Where |
|---|---|
| The pinned baseline, its version, and its source files | `docs/reference.md` §1–§3 |
| Reference is `License: AGPL-3.0-or-later` (verified in the installed distribution's `METADATA`) | `basic_memory-0.23.2.dist-info/METADATA:9` |
| `Store::find_related` "ports the reference recursive CTE **verbatim**" | `plans/auto-memory-rs-execution-plan.md` §0, Phase 9 |
| `src/search/chunking.rs` — "reference chunking port" | same, Phase 8 |
| `src/schema/` — "ports the whole `basic_memory.picoschema` package as five modules" | same, Phase 12–13 |
| `src/markdown/serialize.rs` — PyYAML-compatible emitter, verified byte-for-byte | same, Phase 10 |
| `src/domain/dateparser.rs` — "ports the supported subset" of `dateparser` | same, Phase 15b |
| `src/pycompat.rs` — `python_json_dumps` mirrors `json.dumps(ensure_ascii=True)` | `src/pycompat.rs` |
| Tool schemas, text surfaces, and error strings replayed character-for-character | `tests/golden/**` |

Two consequences follow, and they are the whole reason this document exists:

1. **The AGPL's §13 network clause reaches this code.** Anyone who lets users interact with a
   modified version *over a network* must offer those users the corresponding source. Running
   it as a hosted service does not avoid copyleft the way the plain GPL would.
2. **The license cannot simply be changed.** Relicensing needs the agreement of everyone who
   holds copyright in a derivative of the upstream work — in practice, the upstream authors
   as well as this repository's contributors. A permissive or proprietary relicense is not
   available by editing `Cargo.toml`.

> Practical summary: **AGPL does not prevent selling.** It prevents selling a *closed* product.
> Charging money, shipping binaries, and running it for customers are all permitted; what is
> not permitted is withholding the source from the people who use it over a network.

## 3. What is therefore sellable

| Model | Viable? | Why |
|---|---|---|
| Open-core: AGPL core + separately-licensed enterprise modules | Yes, if the modules are genuinely independent works | The boundary has to be real (separate processes or a clean interface), not a `cfg` flag |
| Paid support / integration / private deployment services | Yes | No copyleft question at all — this is the lowest-risk option |
| Managed hosting, with source offered to users | Yes | §13 is satisfied by publishing the source and the modifications |
| Bundled hardware / appliance deployment | Yes | Same as above; the source obligation travels with the product |
| Closed-source SaaS built on this code | **No** | §13 |
| Proprietary desktop/enterprise product with this code inside | **No** | Distributing the binary under AGPL is fine; keeping it closed is not |
| Relicensing to MIT/Apache to enable the above | **No** | Needs upstream agreement (see §2) |

The last row is the one that matters most for planning: there is no path here to a
conventional proprietary product, so any business plan should assume the source is public.

## 4. Naming and trademark

The project was renamed from `basic-mem-rs` to `auto-memory-rs` (`4462e86`, `c15ed72`,
`400b759`) and its user-visible surface no longer says "Basic Memory": the binary is
`auto-memory`, the MCP server identifies as `auto-memory-rs`, and the diagnostics tool is
`auto_memory_diagnostics`.

Deliberately **not** renamed, because they are upstream contracts rather than this port's
branding:

- references to the reference implementation, its version, its CLI (`basic-memory project add`),
  and its paths, wherever the docs cite where a behavior came from;
- the `.basic-memory/` project-config directory and the `basicMemory` config key, which are
  interoperability surfaces — renaming them would break vaults shared with the reference.

"Basic Memory" and any associated marks belong to the upstream project. Describing this
software as a **port of** or **compatible with** Basic Memory 0.23.2 is factual and is what the
documentation does; implying that it *is* Basic Memory, or that it is endorsed by its authors,
is what the rename was meant to avoid. Keep the compatibility claim descriptive.

## 5. Obligations that ship with the code

Anything built on this repository inherits these, and they belong in a distribution checklist:

- Ship `LICENSE` and a copyright notice with any binary or container image.
- If users reach a modified version over a network, offer them the corresponding source of
  **that** version — including local modifications, not just the upstream tarball.
- The golden corpus in `tests/golden/` is captured reference output, not reference source, but
  it is still part of this repository and travels under the same license.
- `tools/*.py` drive the installed reference implementation at capture time; they are this
  repository's own code under AGPL.

## 6. Open items

- **Legal review** of §2–§3 with the actual upstream authors' position on derivative status.
- **A CLA or DCO**, if dual-licensing or an enterprise tier is ever pursued — without one the
  option is closed by default and gets harder with every merged contribution.
- **A `NOTICE` file** once the above is settled, to carry the upstream attribution text.
