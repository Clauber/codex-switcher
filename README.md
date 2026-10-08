<p align="center">
  <img src="src-tauri/icons/logo.svg" alt="Codex Switcher" width="128" height="128">
</p>

<h1 align="center">AI Usage Tracker + Codex Switcher</h1>

<p align="center">
  Track Claude Code, Zcode, and Codex usage in one dashboard.<br>
  Keep your Codex account switching, warm-ups, and tray controls.
</p>

## Features

- **Multi-provider limits** – Claude subscription windows, model allowances, extra usage, and Zcode Coding Plan 5-hour, weekly, and monthly tool quotas, with remaining percentages and reset times

- **Multi-Account Management** – Add, rename, mask, import, export, and manage multiple Codex accounts in one place
- **Quick Switching** – Switch between accounts from the main window, native tray menu, or tray popup while preserving rotated ChatGPT sessions
- **Usage Stats** – View account usage stats for OAuth accounts, including lifetime tokens, daily buckets, streaks, activity insights, and top integrations
- **Manual Reset Credits** – See available manual reset credits beside each account plan badge, with the closest expiry highlighted as it approaches
- **Automatic Warm-Up** – Warm up one account or all accounts manually, after each 5-hour reset window, or at specific scheduled times of day
- **System Tray Controls** – Use the tray popup to switch accounts, inspect quota and active-account stats, refresh usage, open the main window, or quit the app
- **Tray Display Modes** – Choose between the app icon with session percentage, a text-only hourly/weekly percentage display, or a hidden tray icon
- **macOS Dock Control** – Keep Codex Switcher in the Dock or run it as a menu bar only app, with a first-close prompt and a tray fallback
- **Rate-Limit Monitoring** – View real-time 5-hour session and weekly usage, reset timing, credits, and subscription expiry
- **Blocked Switch Recovery** – Detect running Codex sessions and offer a force-close flow before retrying the account switch
- **Dual Login Mode** – Authenticate with ChatGPT OAuth or import existing `auth.json` files

## Installation

### Download a Release

The easiest way to install Codex Switcher is from the latest GitHub release:

[Download the latest release](https://github.com/Lampese/codex-switcher/releases/latest)

Choose the file for your platform:

- **macOS Apple Silicon:** `Codex.Switcher_*_aarch64.dmg`
- **macOS Intel:** `Codex.Switcher_*_x64.dmg`
- **Windows:** `Codex.Switcher_*_x64-setup.exe` or `Codex.Switcher_*_x64_en-US.msi`
- **Linux Debian/Ubuntu:** `Codex.Switcher_*_amd64.deb`
- **Linux AppImage:** `Codex.Switcher_*_amd64.AppImage`
- **Linux RPM:** `Codex.Switcher-*-1.x86_64.rpm`

> **macOS:** current release builds are not Apple-notarized. If macOS says the
> app is damaged, move it to `/Applications` and remove the quarantine flag:
>
> ```bash
> sudo xattr -dr com.apple.quarantine "/Applications/Codex Switcher.app"
> open "/Applications/Codex Switcher.app"
> ```

### Auto Updates

Codex Switcher checks the latest GitHub release on startup. When a newer signed
update package is available, the app shows an update prompt and can install it
from inside the app.

### Build from Source

#### Prerequisites

- [Node.js](https://nodejs.org/) (v18+)
- [pnpm](https://pnpm.io/)
- [Rust](https://rustup.rs/)

```bash
# Clone the repository
git clone https://github.com/Lampese/codex-switcher.git
cd codex-switcher

# Install dependencies
pnpm install

# Run in development mode
pnpm tauri dev

# Build for production
pnpm tauri build
```

> **Windows:** the `pnpm tauri` script runs through a POSIX shell wrapper
> (`sh ./scripts/tauri.sh`) and will not work in PowerShell/CMD. Use the
> `tauri:win` script instead: `pnpm tauri:win dev` and `pnpm tauri:win build`.

The built application will be in `src-tauri/target/release/bundle/`.

### Run the Dashboard in a Browser

You can also serve the built dashboard over HTTP instead of opening the Tauri shell.

```bash
# Build the frontend and start the web server on 0.0.0.0:3210
pnpm lan
```

Optional environment variables:

- `CODEX_SWITCHER_WEB_HOST` to override the bind host
- `CODEX_SWITCHER_WEB_PORT` to override the port

The browser dashboard serves the same UI and backend actions through `/api/invoke/*`, which makes it usable over LAN, Tailscale, or a remote host tunnel when you expose the chosen port safely.

## Provider usage

The main dashboard reads the signed-in providers on the machine running the backend:

- **Claude Code:** `~/.claude/.credentials.json` (or `CLAUDE_CONFIG_DIR`), with the default macOS Keychain as a fallback. `CLAUDE_CODE_OAUTH_TOKEN` overrides the stored access token. Sign in again in Claude Code if the session has expired; the tracker does not rotate or overwrite its credentials.
- **Zcode:** personal Z.ai and BigModel Coding Plan keys in `~/.zcode/v2/credentials.json`. Encrypted values are read with Zcode's local credential key; run under the same user and, if configured, `ZCODE_CREDENTIAL_SECRET`. `AI_USAGE_ZCODE_CREDENTIALS_FILE` selects another credential file. `ZAI_API_KEY` and `BIGMODEL_API_KEY` are optional overrides. Team and Start Plan billing are not included.

For a login on another machine, add `{"claude_ssh_host":"harness"}` to `~/.codex-switcher/provider-sources.json`. The host must be an existing SSH alias with a trusted host key and passwordless access; the remote machine needs Python 3. The tracker reads only the remote Claude OAuth entry in memory and does not persist it locally.

Checks run every five minutes. Refreshes share a one-minute backend cache, and failed checks back off for five minutes. A failed refresh keeps the last successful snapshot visibly marked as stale; unknown usage is never displayed as zero. Credentials stay in the backend and are never returned to the browser. Provider endpoints and local credential formats may change between releases.

Zcode quota fields follow the [provider's usage-query integration](https://github.com/zai-org/zai-coding-plugins/blob/main/plugins/glm-plan-usage/skills/usage-query-skill/scripts/query-usage.mjs) and [Coding Plan usage documentation](https://zcode.z.ai/en/docs/usage-stats). Claude's subscription usage endpoint is undocumented; parsing supports both legacy windows and the current structured limits list.

The app identity and `~/.codex-switcher` storage directory stay the same, preserving saved Codex accounts. The tray popup shows Claude and Zcode 5-hour/weekly quotas above the Codex accounts; expand more limits for model, spending, and tool quotas. The macOS menu bar includes `Cl` (Claude) and `Z` (Zcode), showing the tighter remaining 5-hour/weekly quota. Expired or unavailable values show `--`.

`pnpm lan` builds the browser backend without desktop GTK/WebKit dependencies. Rust and OpenSSL development headers are still required on Linux. The desktop build remains the default for `pnpm tauri`.

## Usage and Reset Credits

Codex Switcher shows two kinds of account usage information:

- **Rate limits** – the account card shows the current 5-hour and weekly limit
  windows, remaining percentage, reset timing, credit balance, and subscription
  expiry when available.
- **Usage Stats** – ChatGPT OAuth accounts can expand the **Usage
  Stats** panel to view stats such as lifetime tokens,
  today, last 7 days, last 30 days, streaks, longest task, token activity,
  reasoning/activity insights, and most-used integrations. The active account
  opens this panel by default; other accounts keep it collapsed until needed.
- **Manual reset credits** – OAuth accounts with available reset credits show a
  compact badge next to the plan badge. It includes the available count and the
  closest expiry date, hides zero-count results, and turns amber within 10 days
  or red within 3 days of expiry.

The tray popup also includes compact active-account stats for today and
the last 7 days, while keeping the normal rate-limit refresh flow separate.

## Safe Account Switching

ChatGPT can replace an OAuth refresh token after using it. Once replaced, the
older token may no longer be accepted. Before Codex Switcher writes another
account to `~/.codex/auth.json`, it now saves the latest tokens from the account
that is currently active. Switching back therefore restores the current session
instead of an older snapshot.

Token refreshes and account switches are serialized so a background refresh
cannot finish late and overwrite the account you just selected. Codex Switcher
also avoids refreshing the active account while Codex or ChatGPT is running;
close the running app before switching accounts.

If an older Codex Switcher version already saved an invalid refresh token, sign
in to that account again or remove and re-add it once. An invalidated token
cannot be recovered locally.

## macOS Dock and Menu Bar Mode

On macOS, Codex Switcher can either stay visible in the Dock or live only in the
menu bar. The first time you close the main window, the app asks which behavior
you want and lets you choose whether to show that prompt again.

You can change the same setting later from the tray popup or from the native
tray menu under **Dock Icon**. If you choose **Menu Bar Only**, the app keeps a
visible tray item so you can always reopen the main window or switch back to
Dock mode.

## Warm-Up

A warm-up sends one minimal request to an account so its current usage window
has activity before you need it.

- **Manual** – warm up a single or all accounts, from the main window or tray menu.
- **Automatic** – when enabled (per account or for all), the app tracks the
  5-hour window when available and warms it after each reset, as long as the
  weekly limit isn't exhausted. If only the weekly window is available, it
  warms once after the weekly reset and automatically returns to the 5-hour
  schedule if that window reappears.
- **Timed** – pick specific times of day (e.g. `08:00`, `13:00`, `18:00`) from
  the **Timed** control in the main window. At each time the app warms all
  accounts (skipping any whose weekly limit is exhausted), so you control when
  your 5-hour windows start instead of letting them drift.

Timed warm-up checks the schedule every 30 seconds, runs each configured minute
only once per day, and skips missed times if the machine was asleep instead of
warming accounts late.

On macOS you can keep the machine awake with the built-in `caffeinate` command,
which stops automatically when the app quits:

```bash
caffeinate -i -w "$(pgrep -x 'Codex Switcher')"
```

## Disclaimer

This tool is designed **exclusively for individuals who personally own multiple OpenAI/ChatGPT accounts**. It is intended to help users manage their own accounts more conveniently.

**This tool is NOT intended for:**

- Sharing accounts between multiple users
- Circumventing OpenAI's terms of service
- Any form of account pooling or credential sharing

By using this software, you agree that you are the rightful owner of all accounts you add to the application. The authors are not responsible for any misuse or violations of OpenAI's terms of service.

## Updates

This custom build has no in-app updater. Upstream Codex Switcher releases do not contain the provider tracker, so updates must be built and installed from this source. The upstream update feed, installer permissions, and update prompt are removed.

## Versioning

Use the version bump helper to keep app versions in sync across Tauri, Cargo, and the frontend.

```bash
# Exact version
pnpm version:bump 0.2.1

# Semver bumps
pnpm version:patch
pnpm version:minor
pnpm version:major

# Prepare a release commit and tag
# This prompts for a short release note and runs the version bump first.
pnpm release patch

# Prepare and push a release
# The tag stores the release note for the in-app update prompt.
pnpm release patch -- --push

# For non-interactive use, pass the note explicitly.
pnpm release patch -- --push --note "Fixed account switching issues"
```

### Harness account sync and T3 reload

The installed usage tracker can synchronize account switches to a trusted SSH host
and restart its managed T3 service. Opt in with `"codex_t3_ssh_host": "harness"`
in `~/.codex-switcher/provider-sources.json` (preserve existing entries).

The helper requires Python 3.11+, key-based SSH, the `t3code.service` user service,
and T3's default Codex provider using `~/.codex` with file credential storage.
It refuses the switch while any T3 task is running, queued, or waiting for approval.
Once idle, it stops the whole T3 service, writes credentials privately, restarts it,
and waits for a fresh authenticated provider check before selecting the account
on the Mac. A failed verification restores the previous harness credentials.
Credentials travel over SSH stdin and must never be logged. Other SSH targets are
never discovered or selected automatically. There is no scheduled switch or retry.
If the desktop remains on “reconnecting,” reload its window; the remote service
and account have already been checked. Local account storage and other usage
providers retain their existing behavior when the option is absent.
