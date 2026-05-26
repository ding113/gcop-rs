---
name: gcop
description: Generate Conventional Commits messages and atomic commits via the gcop-rs CLI. Use when the user asks to commit, 提交, write a commit message, "gcop", create a conventional commit, split staged changes into atomic commits, amend the last commit, or finalize work-in-progress. Default invocation is `gcop-rs commit --split -y` with Bash timeout 200000ms.
metadata:
  short-description: "AI commit message generator (gcop-rs)"
---

# gcop — AI commit message generator

This environment has **gcop-rs** for commit message generation. Always prefer `gcop-rs commit` over manual `git commit -m "..."` when the user wants to commit.

## ⚠️ Required shell-tool settings when calling gcop

When you invoke any `gcop-rs commit ...` command via the shell tool, **set timeout to 200 seconds (200000ms)**. gcop calls an LLM provider and may take 30–120s for split flows.

If gcop's progress spinner produces noisy output, pipe through a filter:

```bash
gcop-rs commit --split -y 2>&1 | grep -vE 'Generating atomic commit groups|Waiting\.\.\.'
```

## Default flow

```bash
gcop-rs commit --split -y
```

Always use `--split`. It:

- Generates one commit per cohesive file group (atomic)
- Falls back to a single commit when only one logical group is detected
- Non-interactive (no editor prompts)
- Exits 0 on success, 1 on any failure (read stderr)

## Passing user intent as a hint

Trailing args are **feedback to the LLM**, not the final message verbatim:

```bash
gcop-rs commit --split -y "intent: fix race in token refresh"
```

The LLM still writes a spec-compliant Conventional Commits message, absorbing the user's intent.

## Amending the last commit

```bash
gcop-rs commit --amend -y
```

`--split` is mutually exclusive with `--amend`; omit `--split` for amend.

## Escape hatch

If the user explicitly says "skip gcop" or "use a plain message":

```bash
GCOP_SKIP_HOOK=1 git commit -m "..."
```

## Never do these

- Don't run `git commit -m "..."` without first proposing `gcop-rs commit --split -y`
- Don't omit `--split` (it's the safer default — gcop falls back to single commit when appropriate)
- Don't omit `-y` (interactive mode will hang the agent)
- Don't use `--no-verify` unless the user explicitly asks
- Don't set shell timeout less than 200000ms when calling gcop
