# Shared UI shell (owner: Agent G) - round 2

`ui/app.css` + `ui/app.js` + the `<header>` block are owned by G. The header markup is UNCHANGED (brand, six-link `nav`, `.meters`), so review.html, clip.html and analytics.html keep working; only the look changed (dense VMS console).

## Tokens (all on `:root`, names stable; old names kept)

| Token | Value | Use |
| --- | --- | --- |
| `--page` | `#08090A` | page background |
| `--surface` | `#101214` | panels, inputs |
| `--surface2` | `#16191C` | raised/hover |
| `--line` | `#23272B` | 1px hairlines |
| `--line2` | `#343A40` | stronger border / focus-adjacent |
| `--text` | `#E6E8EA` | primary text |
| `--quiet` | `#8A9299` | secondary text |
| `--pop` | `#C5CFCB` | accent / selected |
| `--lead` | `#3F8F8F` | brand teal (non-state) |
| `--support` | `#CE7D36` | warning-ish accent (kept) |
| `--ok` | `#4CC38A` | state: ok |
| `--warn` | `#E5A93C` | state: warn |
| `--bad` | `#E5584D` | state: error |
| `--live` | `#FF3B30` | state: live / recording dot |
| `--edit` | `#0B0D0E` | editor / video wells |
| `--hdr` | `40px` | header height. `review.css` hard-codes `calc(100dvh - 76px)`; change it to `calc(100dvh - var(--hdr))` |
| `--font-ui` | IBM Plex Sans, Segoe UI, system-ui, sans-serif | |
| `--font-mono` | IBM Plex Mono, ui-monospace, Menlo, Consolas, monospace | numerals: use `font-variant-numeric: tabular-nums` |

Root font size is 13px, 15px at >=1800px, 17px at >=2400px. Use `rem` for type and spacing if you want to scale on 4K.

## Utility classes (app.css)

`.btn` (also plain `button`), `.btn.primary`, `.btn.sm`, `.dot` (+ `.live .ok .warn .bad`), `.tag` (+ state colours), `.mono`, `.quiet`, `.hide`, `.sr-only`, `.msg` (+ `.ok`, `.bad`), `.scroll-x` (horizontal scroll inside its own box), `.hair` tables (`table` is compact by default).
Pane rule: no page-level horizontal scroll at 320px; tables go inside `.scroll-x`.
