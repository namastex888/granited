# LocalFleet for macOS

The LocalFleet app is the native macOS front end for LocalFleet and its granited engine (macOS 14+). It puts a menu-bar item next to the notch that shows the engine's state at a glance, and it has a Preferences window laid out like System Settings for managing the engine. There is no web UI.

## Menu-bar item

The icon is the LocalFleet mark as a template image (`LocalFleet/Resources/MenuBarMark.png` and `@2x`), so it follows the light or dark menu bar. A bare binary run outside the `.app` falls back to the `cpu` SF Symbol.

| Icon | Meaning |
|------|---------|
| mark, dimmed | engine stopped or unreachable |
| mark | running and idle |
| mark with a dot badge | serving (some worker has `active_requests > 0`) |
| mark with an arrow badge | a model is loading |
| mark with a warning badge | needs attention: memory stall above 5 %, an unreachable peer, or a failed worker |

The menu is a standard macOS menu. It contains:

- a status line, for example `Running · 2 models ready · 1 request`, followed by any attention reasons
- one line per host from `/internal/hosts`, marked reachable or unreachable, with its round-trip time
- **Start Engine**, **Stop Engine** and **Restart Engine**
- **Settings…** (⌘,), also in the LocalFleet app menu while the window is open
- **Quit LocalFleet** (⌘Q)

**Polling:**
- `GET /healthz` runs every 5 s.
- The heavier endpoints run every 30 s, and every 5 s while Preferences is open: `/admin/models`, `/internal/hosts`, `/internal/hardware`, `/internal/metrics` and `/v1/models`. (`/internal/hosts` makes the router probe every peer.)

## Notch monitor

**Show Notch Monitor** in the menu (on by default) turns on a black strip to the left of the camera housing. It shows the brand mark and the completed-output rate. The strip covers nothing to the right of the camera. Hover or click it to open a panel below the menu bar with Production, Demand and Energy tabs. Escape or a click elsewhere closes it. Screens without a notch get a small pill at the top center.

While the notch is shown, it polls `/internal/metrics` every 2 s, `/internal/hosts` every 5 s, and a local oMLX server's `/api/status` and `/admin/api/activity` every 2 s, if one is configured on loopback in `~/.omlx/settings.json`. Each source has its own freshness, and a slow source doesn't delay the others.

- **Output:** taken only from usage counters an engine reports (`usage` in `/internal/metrics` and `/internal/hosts`). Engines that don't report yet show "Engine update pending".
- **Rate:** completed output over the last 60 s.
- **Today:** counted from the first observation, with gaps shown.
- **Estimate:** appears only after an unbroken hour.
- **Sources:** oMLX is a separate source, covering all of its clients, and is never added to the fleet.
- **Apps:** counted only from requests that carry an `X-LocalFleet-Client` label. These labels are self-declared.
- **Power:** reported per sensor, with its scope. Readings with different scopes are never summed.
- **Memory:** in GiB.

The app keeps only counts and timings, in UserDefaults.

## Preferences

Preferences is a `NavigationSplitView`. The sidebar uses System Settings–style badges, and each pane is a grouped `Form`. While the window is open the app switches to the regular activation policy, so the window comes to the front and shows up in ⌘-Tab. When you close it, the app goes back to being a menu-bar-only accessory.

| Pane | What it shows and does |
|------|------------------------|
| **General** | Status, uptime (`/internal/metrics`), host ID, listen address, endpoint. Start, Stop and Restart. **Start at login** writes `RunAtLoad` into the LaunchAgent plist, then runs `launchctl bootstrap` (on) or `bootout` (off) after you confirm. **Show in Finder** opens the logs. |
| **Models** | One section per local worker: family, device and backend, state, active/max requests, last load time, hold reason, last error. **Load** and **Unload** post the operation and poll `/admin/operations/{id}` until it finishes. Held models can't be loaded. **Keep warm** edits `deployments[].keep_warm` in the profile, then shows *Restart the engine to apply* with a Restart button. |
| **Devices** | Chip, logical CPUs, architecture, load averages; a memory gauge (used/total), available memory, swap, memory stall, the admission reserve; the accelerators. **Hardware plan** comes from `localfleet scan --json` (read-only): the language runtime and models, native workers, and every skipped device with its reason. |
| **Nodes** | This Mac and each peer, with reachability, RTT and model count. **Add Node…** opens a sheet (SSH destination, optional name, optional jump host) that runs `localfleet node add DEST --yes [--name N] [--jump J]` and streams its stdout and stderr live, with Cancel. **Remove…** runs `localfleet node remove NAME` after you confirm. |
| **Connect** | The OpenAI base URL `<url>/v1`; the API key, masked, with Reveal and Copy; copyable curl and Python (`openai`) snippets. The snippets use the first servable chat model from `/v1/models` and fall back to `granite-4.2-8b`. They read the key from `$GRANITE_API_KEY`, so it never appears in them. |
| **Advanced** | The profile path (Reveal in Finder, Open); a **Memory reserve** stepper in GiB that writes `ram_reserve_bytes` (restart to apply); the checkout, models, logs, Python and localfleet locations. **Uninstall Engine…** runs `localfleet uninstall` after a destructive-style confirmation. |

**How profile edits work:**
- Keep warm and Memory reserve edit the profile JSON in place, by byte range. Key order, indentation, escapes and the trailing newline all stay as they were; only the value changes, or the key is appended after its last sibling.
- The result is validated as JSON before an atomic write, and the file keeps its permissions.

**Keys:**
- The admin key is sent only as a bearer header. It is never displayed or logged.
- The API key appears only after you click Reveal.

## Configuration

The app reads `~/Library/Application Support/LocalFleet/app.json`, which `localfleet install` writes (the pre-rename `…/granited/app.json` is still read). To use a different file, pass `--config PATH` or set `LOCALFLEET_CONFIG`. Every field is optional; a feature whose field is missing is disabled and says why.

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
  "cli":        "/abs/checkout/scripts/localfleet.py",
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
| `root` | the checkout; localfleet runs with it as the working directory. When `root` is set, `cli` and `models` default to `<root>/scripts/localfleet.py` and `<root>/models` if those exist. |
| `profile` | the engine profile edited by Keep warm and Memory reserve |
| `python` | interpreter for localfleet; default `/usr/bin/python3` |
| `dry_run` | when `true`, no launchctl or localfleet command runs and no LaunchAgent plist is written; each prints its argv instead. `--dry-run` on the command line does the same. `scan` is read-only and always runs. |

The app re-reads `app.json`, the keys file, the profile and the plist on every 5 s tick, so you don't need to restart it after they change. Plain-http hosts on your LAN are allowed (`NSAllowsLocalNetworking`).

## Engine commands

| Action | Command (argv, never through a shell) |
|--------|---------------------------------------|
| Start | `launchctl bootstrap gui/<uid> <plist>` |
| Stop | `launchctl bootout gui/<uid>/<label>` |
| Restart | `launchctl kickstart -k gui/<uid>/<label>` |
| Start at login on / off | write `RunAtLoad`, then bootstrap / bootout |
| Add node | `<python> <cli> node add DEST --yes [--name N] [--jump J]` |
| Remove node | `<python> <cli> node remove NAME` |
| Uninstall | `<python> <cli> uninstall` |
| Hardware plan | `<python> <cli> scan --json` |

- Stop, Restart, Unload, Start at login, Remove and Uninstall ask for confirmation first, and the dialog shows the exact command.
- Values that reach argparse or ssh (destination, name, jump host) are rejected if they start with `-` or contain whitespace.

## Build

```sh
apps/macos/build-app.sh          # swift build -c release + ad-hoc signed build/LocalFleet.app
open apps/macos/build/LocalFleet.app
```

You need Xcode or the Swift 6 toolchain. The package is in `LocalFleet/` (SwiftPM, one executable target, Swift 6 language mode).

## Command line and headless checks

```sh
bin=apps/macos/build/LocalFleet.app/Contents/MacOS/LocalFleet
$bin --config PATH --open-preferences [--pane devices] [--dry-run]   # GUI; prints the window id
$bin --self-test [--config PATH] [--url URL] [--keys PATH]           # GET-only checks, exit 0 on PASS
$bin --dry-run-node-add user@gpu-box --name gpu --jump bastion       # print the node add argv
$bin --dry-run-node-remove NAME | --dry-run-uninstall
$bin --write-keep-warm MODEL on|off --profile PATH                   # edit a profile headless
$bin --write-ram-reserve GIB --profile PATH
$bin --write-run-at-load on|off --plist PATH                         # plist only, no launchctl
$bin --scan                                                          # localfleet scan --json, summarized
$bin --render-icons DIR                                              # the menu-bar icon per state, light and dark PNGs
$bin --exercise-load MODEL --url URL                                 # POST load, wait, POST unload, wait
```

- **`--self-test`** sends only GET requests: `/healthz`, `/admin/models`, `/internal/hosts`, `/internal/hardware`, `/internal/metrics` and `/v1/models`. It prints the launchctl and localfleet commands the GUI would run, without running them.
- **`--exercise-load`** refuses port 8310, so it can't post to a live router by accident.
- **`--open-preferences`** prints `preferences window <id> pane <name>`. The id is the CGWindowID that `screencapture -l <id>` takes.
