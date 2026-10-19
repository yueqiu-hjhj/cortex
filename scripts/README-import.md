# Headless migration: Anki & Obsidian → Cortex

Two zero-dependency scripts that move an existing collection into Cortex
**without opening the app** — for bulk migrations, scripts, and CI.

| Script | Input | Writes |
|---|---|---|
| `cortex-import-anki.mjs` | Anki `.apkg` | one subject per deck, a `flashcards` material, and SRS rows due now |
| `cortex-import-obsidian.mjs` | Obsidian vault (`*.md`) | one subject, notes, and a rebuilt `[[double-link]]` index |

They write directly into the same `cortex.db` the desktop app uses, mirroring
the insert contracts in `src-tauri/src/repo.rs` (UUID ids, ms timestamps,
`position = MAX+1`, flashcard payload `[{"q","a"}]`, note-link slugs).

## Requirements

- **Node ≥ 22** (uses the built-in `node:sqlite`; no `npm install` needed).
- **Cortex installed and launched once**, so the database exists and all
  migrations have run.
- **Cortex closed while migrating** — SQLite is single-writer; a running app
  will make the write wait and then fail.

## Usage

```bash
# Anki: one subject per deck, cards become reviewable flashcards
node scripts/cortex-import-anki.mjs biology.apkg

# Obsidian: one subject for the vault, notes keep their [[links]]
node scripts/cortex-import-obsidian.mjs ~/Documents/MyVault
```

### Options (both)

| Flag | Meaning |
|---|---|
| `--db <path>` | Target `cortex.db` (default: the app's data dir) |
| `--subject <name>` | Merge everything into one subject with this name |
| `--dry-run` | Parse and report only — writes nothing |
| `--help` | Usage |

### Anki-only

| Flag | Meaning |
|---|---|
| `--deck <name>` | Import just this deck (exact name) |

### Obsidian-only

| Flag | Meaning |
|---|---|
| `--by-folder` | One subject per top-level folder instead of one per vault |

### Database resolution

1. `--db <path>`
2. `$CORTEX_DB`
3. `$CORTEX_DATA_DIR/cortex.db`
4. The app-data dir: `%APPDATA%\app.study.cortex` (Windows),
   `~/Library/Application Support/app.study.cortex` (macOS),
   `$XDG_DATA_HOME/app.study.cortex` (Linux)

## What lands where

**Anki**

```
deck "Biology"                → subject "Biology" (reused if it already exists)
cards                         → materials row, kind='flashcards', payload [{"q","a"}]
each card                     → srs_cards row (kind='flashcard', due_at = now)
```

Card text is cleaned exactly like `src-tauri/src/anki.rs` — HTML stripped,
entities decoded (`&nbsp;` → space, `&#x27;` → `'`), whitespace collapsed — so
script and in-app imports produce identical fronts/backs. Cards with an empty
front or back are skipped; duplicate fronts within a deck are deduped
(case-insensitive).

**Obsidian**

```
vault folder                  → subject (vault name, or top folder with --by-folder)
each .md                      → notes row (title = frontmatter `title` or file name)
[[target]] / [[target|alias]] → note_links row (slug = lowercased target)
```

Frontmatter is parsed for `title` (only); the body is stored verbatim so
Markdown and `[[links]]` survive. `.obsidian/`, `.trash/`, `.git/`, and
`node_modules/` are skipped. Notes whose title already exists in the subject are
skipped (same-title notes are ambiguous to the link resolver).

## Safety

- Each run is a single SQLite transaction — a failure rolls back cleanly.
- Re-running is safe: an already-migrated deck (same material title) or an
  existing note title is skipped instead of duplicated.
- `--dry-run` parses everything and prints the plan without touching the DB.

## Troubleshooting

| Message | Fix |
|---|---|
| `找不到数据库` | Launch Cortex once, or pass `--db /path/to/cortex.db` |
| `数据库缺少表` | Update/launch the app once so migrations run |
| `zstd 压缩的新版格式` | In Anki, export with **Support older Anki versions** checked |
| `数据库不可写` | Close Cortex, then retry |
| `不是有效的 .apkg` | The file is not a ZIP (check it is a real `.apkg`) |

## Self-test

A repeatable end-to-end check (builds a temp DB, a deflated `.apkg`, and a small
vault, then runs both importers):

```bash
node scripts/import-selftest.mjs
```

It prints `ALL CHECKS PASSED` on success and exits non-zero on failure.
