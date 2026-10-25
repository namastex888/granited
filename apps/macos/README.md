# GraniteBar

GraniteBar is the native macOS front end for granited (macOS 14+). It puts a menu-bar item next to the notch that shows the engine's state at a glance, and it has a Preferences window laid out like System Settings for managing the engine. There is no web UI.

## Menu-bar item

The icon is a template SF Symbol, so it follows the light or dark menu bar:

| Icon | Meaning |
|------|---------|
| `cpu`, dimmed | engine stopped or unreachable |
| `cpu` | running and idle |
| `cpu.fill` | serving (some worker has `active_requests > 0`) |
| `cpu` with an arrow badge | a model is loading |
| `cpu` with a warning badge | needs attention: memory stall above 5 %, an unreachable peer, or a failed worker |

The menu is a standard macOS menu. It contains:

- a status line, for example `Running · 2 models ready · 1 request`, followed by any attention reasons
- one line per host from `/internal/hosts`, marked reachable or unreachable, with its round-trip time
- **Start Engine**, **Stop Engine** and **Restart Engine**
- **Preferences…** (⌘,)
- **Quit GraniteBar** (⌘Q)

**Polling:**
- `GET /healthz` runs every 5 s.
- The heavier endpoints run every 30 s, and every 5 s while Preferences is open: `/admin/models`, `/internal/hosts`, `/internal/hardware`, `/internal/metrics` and `/v1/models`. (`/internal/hosts` makes the router probe every peer.)

## Preferences

Preferences is a `NavigationSplitView`. The sidebar uses System Settings–style badges, and each pane is a grouped `Form`. While the window is open the app switches to the regular activation policy, so the window comes to the front and shows up in ⌘-Tab. When you close it, the app goes back to being a menu-bar-only accessory.

| Pane | What it shows and does |
|------|------------------------|
| **General** | Status, uptime (`/internal/metrics`), host ID, listen address, endpoint. Start, Stop and Restart. **Start at login** writes `RunAtLoad` into the LaunchAgent plist, then runs `launchctl bootstrap` (on) or `bootout` (off) after you confirm. **Show in Finder** opens the logs. |
| **Models** | One section per local worker: family, device and backend, state, active/max requests, last load time, hold reason, last error. **Load** and **Unload** post the operation and poll `/admin/operations/{id}` until it finishes. Held models can't be loaded. **Keep warm** edits `deployments[].keep_warm` in the profile, then shows *Restart the engine to apply* with a Restart button. |
| **Devices** | Chip, logical CPUs, architecture, load averages; a memory gauge (used/total), available memory, swap, memory stall, the admission reserve; the accelerators. **Hardware plan** comes from `granitectl scan --json` (read-only): the language runtime and models, native workers, and every skipped device with its reason. |
| **Nodes** | This Mac and each peer, with reachability, RTT and model count. **Add Node…** opens a sheet (SSH destination, optional name, optional jump host) that runs `granitectl node add DEST --yes [--name N] [--jump J]` and streams its stdout and stderr live, with Cancel. **Remove…** runs `granitectl node remove NAME` after you confirm. |
| **Connect** | The OpenAI base URL `<url>/v1`; the API key, masked, with Reveal and Copy; copyable curl and Python (`openai`) snippets. The snippets use the first servable chat model from `/v1/models` and fall back to `granite-4.2-8b`. They read the key from `$GRANITE_API_KEY`, so it never appears in them. |
| **Advanced** | The profile path (Reveal in Finder, Open); a **Memory reserve** stepper in GiB that writes `ram_reserve_bytes` (restart to apply); the checkout, models, logs, Python and granitectl locations. **Uninstall Engine…** runs `granitectl uninstall` after a destructive-style confirmation. |

**How profile edits work:**
- Keep warm and Memory reserve edit the profile JSON in place, by byte range. Key order, indentation, escapes and the trailing newline all stay as they were; only the value changes, or the key is appended after its last sibling.
- The result is validated as JSON before an atomic write, and the file keeps its permissions.

**Keys:**
- The admin key is sent only as a bearer header. It is never displayed or logged.
- The API key appears only after you click Reveal.

## Configuration

The app reads `~/Library/Application Support/granited/app.json`, which `granitectl install` writes. To use a different file, pass `--config PATH` or set `GRANITE_BAR_CONFIG`. Every field is optional; a feature whose field is missing is disabled and says why.

```json
{
  "url":        "http://127.0.0.1:8310",
  "keys":       "/abs/checkout/state/secrets/keys.json",
  "label":      "ai.granited.router",
  "plist":      "~/Library/LaunchAgents/ai.granited.router.plist",
  "logs":       "/abs/checkout/state",
  "root":       "/abs/checkout",
  "profile":    "/abs/checkout/configs/router.json",
  "python":     "/abs/checkout/.venv-native/bin/python",
  "granitectl": "/abs/checkout/scripts/granitectl.py",
  "models":     "/abs/checkout/models",
  "dry_run":    false
}
```

| Field | Meaning and default |
|-------|---------------------|
| `url` | engine URL; default `http://127.0.0.1:8310` |
| `keys` | the JSON secrets file. `GRANITE_ADMIN_KEY` authorizes `/admin` and `/internal`; `GRANITE_API_KEY` is what the Connect pane shows. The app warns if the file is not mode `0600`. |
| `label`, `plist` | the LaunchAgent; defaults `ai.granited.router` and `~/Library/LaunchAgents/<label>.plist` |
| `logs` | state directory opened by Show Logs |
| `root` | the checkout; granitectl runs with it as the working directory. When `root` is set, `granitectl` and `models` default to `<root>/scripts/granitectl.py` and `<root>/models` if those exist. |
| `profile` | the engine profile edited by Keep warm and Memory reserve |
| `python` | interpreter for granitectl; default `/usr/bin/python3` |
| `dry_run` | when `true`, no launchctl or granitectl command runs and no LaunchAgent plist is written; each prints its argv instead. `--dry-run` on the command line does the same. `scan` is read-only and always runs. |

The app re-reads `app.json`, the keys file, the profile and the plist on every 5 s tick, so you don't need to restart it after they change. Plain-http hosts on your LAN are allowed (`NSAllowsLocalNetworking`).

## Engine commands

| Action | Command (argv, never through a shell) |
|--------|---------------------------------------|
| Start | `launchctl bootstrap gui/<uid> <plist>` |
| Stop | `launchctl bootout gui/<uid>/<label>` |
| Restart | `launchctl kickstart -k gui/<uid>/<label>` |
| Start at login on / off | write `RunAtLoad`, then bootstrap / bootout |
| Add node | `<python> <granitectl> node add DEST --yes [--name N] [--jump J]` |
| Remove node | `<python> <granitectl> node remove NAME` |
| Uninstall | `<python> <granitectl> uninstall` |
| Hardware plan | `<python> <granitectl> scan --json` |

- Stop, Restart, Unload, Start at login, Remove and Uninstall ask for confirmation first, and the dialog shows the exact command.
- Values that reach argparse or ssh (destination, name, jump host) are rejected if they start with `-` or contain whitespace.

## Build

```sh
apps/macos/build-app.sh          # swift build -c release + ad-hoc signed build/GraniteBar.app
open apps/macos/build/GraniteBar.app
```

You need Xcode or the Swift 6 toolchain. The package is in `GraniteBar/` (SwiftPM, one executable target, Swift 6 language mode).

## Command line and headless checks

```sh
bin=apps/macos/build/GraniteBar.app/Contents/MacOS/GraniteBar
$bin --config PATH --open-preferences [--pane devices] [--dry-run]   # GUI; prints the window id
$bin --self-test [--config PATH] [--url URL] [--keys PATH]           # GET-only checks, exit 0 on PASS
$bin --dry-run-node-add user@gpu-box --name gpu --jump bastion       # print the node add argv
$bin --dry-run-node-remove NAME | --dry-run-uninstall
$bin --write-keep-warm MODEL on|off --profile PATH                   # edit a profile headless
$bin --write-ram-reserve GIB --profile PATH
$bin --write-run-at-load on|off --plist PATH                         # plist only, no launchctl
$bin --scan                                                          # granitectl scan --json, summarized
$bin --render-icons DIR                                              # the menu-bar icon per state, light and dark PNGs
$bin --exercise-load MODEL --url URL                                 # POST load, wait, POST unload, wait
```

- **`--self-test`** sends only GET requests: `/healthz`, `/admin/models`, `/internal/hosts`, `/internal/hardware`, `/internal/metrics` and `/v1/models`. It prints the launchctl and granitectl commands the GUI would run, without running them.
- **`--exercise-load`** refuses port 8310, so it can't post to a live router by accident.
- **`--open-preferences`** prints `preferences window <id> pane <name>`. The id is the CGWindowID that `screencapture -l <id>` takes.
