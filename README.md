# CI lights

Track a pushed PR's checks on the agent's Haunt row and get pinged when they settle.

When an agent runs `git push`, CI lights asks `gh` for the branch's PR and then watches its checks. The row shows `CI 7/9` in the label slot (orange once a check fails) and `#142 · checks running` after the meta line. When a check fails, or once every check has passed, you get one notification: `api · #142 checks failed` with `lint (clippy) failed after 3m12s`, or `api · #142 checks passed` with `all 9 passed in 4m7s`. **Open PR** opens the PR on github.com in your browser and **Open agent** brings the tab up. Haunt runs `gh` itself, in the folder the tab was opened in and only inside your home folder; when `gh` can't answer (no login, no PR), the plugin's log says why once.

The result stays on the row until the agent's next push. Skipped checks don't count, and runs that started before the push belong to the previous commit and are left out. If no PR or no checks turn up within 10 minutes of the push, or the checks are still running after two hours, CI lights stops asking. While the agent needs you, Haunt's own status takes the row.

## Settings

```toml
[plugins."dev.haunt.ci-lights"]
poll_secs = 30        # seconds between looks at the checks, 15 to 600
notify_failed = true
notify_passed = true
unverified = false    # also follow pushes from events without the tab's nonce
```

## Capabilities

| capability | why |
|---|---|
| `events = ["agent.tool"]` | to see each Bash call |
| `event_details` | the Bash command, to spot `git push`; haunt asks in red, since commands can hold secrets |
| `exec = ["gh-pr-view", "gh-pr-checks"]` | `gh pr view` and `gh pr checks` in the tab's folder, with fixed arguments; needs `gh` signed in |
| `decorate` | the label and note on the row |
| `notifications` | the ping when checks settle |
| `open_url = ["github.com"]` | Open PR on the notification; Haunt opens only github.com links, and only right after you click |
| `timer` | asks again every `poll_secs` while checks run, and stops when they settle |

## Build and test

```sh
cargo build --release --target wasm32-wasip2   # or ./build.sh
cargo test                                     # unit tests
haunt plugin install .
```

`e2e/ci_lights.rs` runs the built plugin against a real Haunt core: a push in a fresh repo makes Haunt run the real `gh` in the tab's own folder (offline, with no login, so it fails there), a push from a dot-folder isn't run at all, and a forged push runs nothing. Copy it into a Haunt checkout's `cli/tests/` and run `HAUNT_PLUGINS_DIR=<this repo's parent> cargo test -p haunt-cli --features plugins --test ci_lights`.

Needs Haunt with plugin API 0.5 (`api = "0.5"`) and its `exec` import.

## Licence

MIT, see `LICENSE`.
