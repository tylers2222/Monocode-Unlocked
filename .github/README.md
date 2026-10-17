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
| **Mono to Mono**         | `messages.get_monos`, `messages.send` and `messages.reply` let a Mono find other Monos, message them and answer them; `monos.read` reads their chats.             | Not proposed                                                         |
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
monocode app messages.get_monos --json '{}'
monocode app messages.send --json '{"message":"Can you check CI on vibtra?","send_to":"<mono_id>","wanting_reply":true,"intent":"new"}'
monocode app messages.reply --json '{"message":"CI is green."}'
```

- The message arrives in the other Mono's chat as a `<mono_message>` tag that names the sender and says whether it wants a reply.
- `wanting_reply` and `intent` are required. `intent: "new"` waits for a running turn to end. `intent: "steer"` with `reply_to` (the `message_id` of a message you sent) reaches the turn that runs that message's work, and is refused with `target_gone` when that work is not running. A Giga can pass `scope: "turn"` instead of `reply_to` to steer everything a Mono is doing now.
- A reply or a steer starts with one line that names the message it refers to, such as `[steer → #1 · "first words…"]`, and the tag carries that message's id as `reply_to`. A whole-turn steer starts with `[steer → whole turn]`.
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
