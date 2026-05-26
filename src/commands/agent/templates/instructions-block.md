## Commit messages (auto-installed by gcop-rs)

This environment has the `gcop-rs` CLI for Conventional Commits generation.
**Always** prefer it over `git commit -m "..."`:

```bash
gcop-rs commit --split -y
```

- `--split` (default): atomic per-cohesive-group commits
- `-y`: non-interactive, required (interactive mode hangs the agent)
- When calling via Bash / shell tool, **set `timeout: 200000`** (200 seconds; gcop calls an LLM)
- If spinner output is noisy: pipe through `2>&1 | grep -vE 'Generating atomic commit groups|Waiting\.\.\.'`

Hint mode (trailing args = LLM feedback, not the literal subject):

```bash
gcop-rs commit --split -y "intent: <one-line description>"
```

Amend (omit `--split` for amend):

```bash
gcop-rs commit --amend -y
```

Skip gcop only when the user explicitly asks:

```bash
GCOP_SKIP_HOOK=1 git commit -m "..."
```

See the `gcop` skill for the full reference.
