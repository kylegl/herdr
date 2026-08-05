# Domain docs

This repository uses a **single-context** domain layout.

Before exploring the codebase, read `.agents/CONTEXT.md` when it exists and relevant ADRs under `.agents/adr/`. Proceed silently when those files do not exist; domain-modeling creates them when vocabulary or decisions are resolved.

## Layout

```text
.agents/
├── CONTEXT.md
└── adr/
```

Use terms from `.agents/CONTEXT.md` consistently and surface conflicts with recorded ADRs instead of silently overriding them.
