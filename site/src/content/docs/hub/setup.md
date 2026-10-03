---
title: Set up a hub
description: Run a hub, join machines to it, and put TLS in front of it.
---

import { Steps, Tabs, TabItem } from '@astrojs/starlight/components';

<Steps>

1. **Start the hub** on the machine that will hold the archive.

   ```sh
   asm hub serve --port 7434
   ```

   ```text
   asm hub on http://127.0.0.1:7434/  (ctrl-c to stop)
     join with  ASM_JOIN_TOKEN=asmj_… asm join http://127.0.0.1:7434
   ```

   The hub binds **loopback** by default. See [exposing it](#exposing-the-hub) below.

2. **Join each machine.** The token comes from the environment, so it never appears in the process list:

   ```sh
   ASM_JOIN_TOKEN=asmj_… asm join https://hub.example.ts.net
   # or, from a secrets manager:
   my-secret-tool get asm-token | asm join https://hub.example.ts.net --token -
   ```

   Pass `--name laptop` to name the machine to the others (default: its hostname). The join exchanges the one-time token for this machine's **own credential**, stored in `hub-client.json` (mode `0600`).

3. **Push** what is already here:

   ```sh
   asm push --all
   ```

4. **Check** how you stand, from either machine:

   ```sh
   asm remote list
   asm remote machines
   ```

</Steps>

## Exposing the hub

The hub speaks HTTP and **you provide TLS**.

<Tabs>
  <TabItem label="Tailscale">
    Keep the hub on loopback and publish it with HTTPS:

    ```sh
    asm hub serve                # 127.0.0.1:7434
    tailscale serve --bg 7434    # https://<machine>.<tailnet>.ts.net
    ```

    Join with the `https://` URL.
  </TabItem>
  <TabItem label="Reverse proxy">
    Put Caddy or nginx in front, terminate TLS there, and forward to `127.0.0.1:7434`. Large sessions upload in a single request body, so don't set a small body limit on the proxy.
  </TabItem>
  <TabItem label="Trusted LAN">
    ```sh
    asm hub serve --host 192.168.1.10
    ```

    `asm join` accepts plain HTTP to loopback, private (RFC 1918) and VPN (CGNAT) addresses, and refuses it for anything else unless you pass `--insecure-http`.
  </TabItem>
</Tabs>

:::caution
`--host 0.0.0.0` on a machine with a public address puts the hub on the internet over plain HTTP. Don't.
:::

## Managing machines and the token

```sh
asm hub machines            # who has joined, and when they were last seen
asm hub revoke laptop       # by id or unique name: shuts one machine out
asm hub token               # reprint the join command
asm hub token --rotate      # retire the join token; joined machines keep access
```

The hub keeps only a hash of each credential, so `revoke` removes one laptop without rotating the rest. For a web page that does the same and more, see [Administering the hub](/hub/admin/).

## Limits

`asm hub serve --max-file-mb 4096` caps the largest single file the hub accepts (default 4 GiB). Uploads stream to disk and are verified by hash before they are kept, so a transcript of hundreds of megabytes costs the hub a few megabytes of memory.

## Where things live

On the hub, under `<data>/hub/`: `hub.json`, `machines.json` (credential hashes), `blobs/` (content-addressed files) and `sessions/<agent>/<id>/` (every revision of every session). On each machine: `hub-client.json` and `hub-state.json`. See [Data files](/reference/data-files/).
