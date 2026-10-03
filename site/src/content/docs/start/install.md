---
title: Install
description: Install the asm binary, update it, or build it from source.
---

`asm` is one self-contained binary: SQLite is bundled and the web UI's assets are embedded. There is nothing else to install.

## One line

```sh
curl -fsSL https://raw.githubusercontent.com/samishal1998/agent-sessions-manager/main/install.sh | sh
```

This downloads the binary for your platform (Linux or macOS, x86_64 or arm64) from the latest release, checks it against the release's `SHA256SUMS`, and installs it to `~/.local/bin`. A failed download or a checksum mismatch stops before anything is written, so an existing install is never left broken.

Make sure `~/.local/bin` is on your `PATH`, then:

```sh
asm --version
asm            # on a terminal this opens the TUI; piped, it prints a table
```

| Variable | What it does |
|---|---|
| `ASM_VERSION` | Install a specific tag instead of the latest. |
| `ASM_INSTALL_DIR` | Where the binary goes (default `~/.local/bin`). |
| `ASM_BASE_URL` | Fetch assets from a mirror or a local directory. |
| `GITHUB_TOKEN` | Only needed if the repository is ever made private again. |

## Update

```sh
asm update --check   # say whether a newer release exists
asm update           # verify against SHA256SUMS, then replace the running binary
```

A bad download leaves the working binary alone, and `asm update` refuses to install something it could not verify. It needs `curl` or `wget`, the same as the installer. It also refuses to replace a cargo build directory's binary.

## Build from source

```sh
# The web UI's assets are embedded at compile time, so build them first.
cd crates/asm-web/frontend && bun install && bun run build && cd -
cargo build --release
```

You need a recent stable Rust (developed on 1.94) and, for the web UI only, Node or Bun.

:::note[Releases]
A release is made by tagging: `git tag v0.1.0 && git push origin v0.1.0` builds Linux and macOS binaries for x86_64 and arm64, publishes them with checksums, and is what `install.sh` reads. The workflow can also be dispatched without a tag to dry-run the build.
:::

## What was verified

asm is checked against real installs of each agent — "verified" meaning a real session was imported or restored and then resumed in the target agent's own CLI with its conversation intact. [Agents](/reference/agents/) lists the versions. `asm doctor` warns when your installed versions have drifted from them.
