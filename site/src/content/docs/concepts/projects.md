---
title: Projects
description: Why a project is a git repository and not a directory.
---

A project is a **git repository**, not a directory. Every worktree of a repository is the same project, and so is a session started in a subdirectory of one — an agent whose working directory wandered into `crates/foo` has not started working on a different codebase. Sessions from different agents in the same repository share one project too.

Identity comes from `git rev-parse --git-common-dir`, which every worktree of a repository agrees on. Directories outside any repository stand alone. Worktrees with no sessions are still listed (`asm projects --worktrees`, `asm worktrees`), so an idle checkout is visible rather than missing.

## Across machines

On a hub, sessions are grouped by **git origin URL** first, then by the portable (`${HOME}`-relative) form of the path, so the same repository checked out at different paths on two machines is one project in `asm remote list`. Where a pull *lands* is decided by the path (same place relative to `$HOME`, or `--project-dir`), not by origin.
