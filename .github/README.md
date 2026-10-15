<p align="center">
  <img src="../public/monocode.png" alt="MonoCode" width="88" />
</p>

<h1 align="center">Monocode Unlocked</h1>

<p align="center">
  <strong><a href="https://github.com/hardbeat920/monocode">MonoCode</a>, plus the orchestration unlocks that upstream leaves out.</strong>
</p>

This repo is upstream [`hardbeat920/monocode`](https://github.com/hardbeat920/monocode) with a small stack of extra commits on top. It rebases onto upstream every night, so it has every upstream feature and fix. For install, setup and general use, read the [upstream README](../README.md).

## Unlocks

| Unlock                   | What it adds                                                                                                                                                     | Upstream                                                             |
| ------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------- |
| **Steer a busy session** | `sessions.send` with `"whenBusy":"steer"` puts a prompt into the session's running turn, the same as Steer in the app.                                           | Proposed in [#979](https://github.com/hardbeat920/monocode/pull/979) |
| **Queue a follow-up**    | `sessions.send` with `"whenBusy":"followUp"` adds the prompt to the session's visible queue. You can edit or remove it there, and it is sent when the turn ends. | Proposed in [#979](https://github.com/hardbeat920/monocode/pull/979) |
| **Mono to Mono**         | `monos.list`, `monos.read` and `monos.send` let a Mono find other Monos, read their chats and send them messages. `whenBusy` works here too.                     | Not proposed                                                         |
| **Readable dev builds**  | `npm run tauri dev` runs Tailwind's CSS optimize pass, so selected tabs, chips and glass panes are not white on white. Release builds do not change.             | Proposed in [#980](https://github.com/hardbeat920/monocode/pull/980) |

### Steer and follow-up

```sh
monocode app sessions.send --json '{"sessionId":"<id>","prompt":"Use Postgres, not SQLite","whenBusy":"steer"}'
# {"ok":true,"result":{"delivery":"steered", ...}}

monocode app sessions.send --json '{"sessionId":"<id>","prompt":"Then add tests","whenBusy":"followUp"}'
# {"ok":true,"result":{"delivery":"queued","queuedMessageId":"...", ...}}
```

- Without `whenBusy`, a busy session returns an error, as in upstream.
- An idle session runs the prompt as a new turn, whatever the mode.
- A harness that cannot steer mid-turn returns an error. The prompt is never put in the queue in its place.
- `--request-id` retries never deliver the same prompt twice.
- `notifyOnComplete:true` reports when the turn that uses the prompt ends.

### Mono to Mono

```sh
monocode app monos.list --json '{}'
monocode app monos.send --json '{"monoId":"<id>","prompt":"Can you check CI on vibtra?","whenBusy":"followUp"}'
```

- The message arrives in the other Mono's chat, marked as coming from the sender.
- A Mono cannot send to itself.

## How it stays current

- `tyler/main` is upstream `main` plus the commits that add the unlocks.
- Every night at 13:00 UTC, [`fork-sync.yml`](workflows/fork-sync.yml) rebases those commits onto upstream, runs `npm run check`, and pushes.
- If the rebase conflicts or a check fails, nothing is pushed, and the job opens an issue named "Nightly upstream sync failed".
- When upstream merges an unlock, the rebase drops that commit, because upstream already has it.

## Build it

```sh
git clone https://github.com/tylers2222/Monocode-Unlocked.git
cd Monocode-Unlocked
npm install
npm run tauri build
```

See the [upstream contributing guide](../CONTRIBUTING.md) for the toolchain it needs.

## License

Same as upstream. See [LICENSE](../LICENSE).
