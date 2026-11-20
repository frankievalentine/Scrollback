# Scrollback

Every coding-agent session on your computer in one place, each with a short
summary of what happened and what was left open.

Scrollback reads the session history that Claude Code, Codex, OpenCode, pi
and Cursor already keep, indexes it, and writes a summary beside each session,
so you can find last Tuesday's work by what it was about instead of
scrolling through transcripts.

## Features

- **A small, fast native app.** Built in Rust with [GPUI](https://www.gpui.rs),
  the UI framework behind the Zed editor. It opens with your sessions already
  on screen and stays out of the way in the menu bar.
- **Fast search.** Type anywhere in the window to search titles, projects,
  summaries and every message in every session, with results as you type.
- **Summaries by default.** Once a session goes quiet, Scrollback writes a
  title, a short summary, how it ended and anything left to pick up. Sessions
  you are still working in are summarised as you go.
- **Transcripts grouped by step.** Open a session to read it as the steps the
  summary describes, each folding open to the turns behind it.
- **One-step resume.** Copy the command that reopens a session in the tool it
  came from.
- **Activity.** Your week at a glance: time in use each day, which tools you
  used, and the open ends still waiting.
- **Menu bar popover.** Recent summaries and the week's activity, a click
  away, without opening the window.
- **Read-only and local.** Scrollback never writes to your tools' files, and
  its index stays on your computer.

## Sources

| Source | Read from |
| --- | --- |
| Claude Code, and the Claude app's coding sessions | `~/.claude/projects` |
| Codex, and the ChatGPT app's Codex sessions | `~/.codex` |
| OpenCode | `~/.local/share/opencode` |
| pi | `~/.pi/agent/sessions` |
| Cursor | Cursor's own data folder |
| T3 Code | labels the Claude Code and Codex sessions it started |

Each folder can be changed in Settings → Sources, and any source can be
hidden there.

## Getting started

Scrollback is built from source for now. You need
[Rust](https://rustup.rs) (stable) and, on a Mac, the Xcode command line tools
(`xcode-select --install`).

### macOS

```sh
git clone <this repository> scrollback
cd scrollback
scripts/bundle.sh --install
```

This builds `Scrollback.app`, installs it in `/Applications` and signs it for
this Mac. Open it from Launchpad or Spotlight.

### Linux

Install the libraries GPUI builds against (Debian and Ubuntu shown):

```sh
sudo apt install pkg-config cmake clang libxkbcommon-dev libxkbcommon-x11-dev \
  libwayland-dev libx11-xcb-dev libxcb1-dev libfontconfig-dev libfreetype-dev \
  libssl-dev libzstd-dev libvulkan-dev libasound2-dev
```

Then build a package and install it:

```sh
scripts/package-linux.sh
sudo apt install ./target/package/scrollback_*.deb
```

Or unpack `target/package/scrollback-*.tar.gz` and run `./install.sh` to
install into `~/.local` without root.

### Windows

```powershell
pwsh scripts/package-windows.ps1
```

This leaves a zip in `target\package`, and an installer too if
[Inno Setup](https://jrsoftware.org/isinfo.php) is installed.

Linux and Windows builds are newer than the Mac one and have had less use.

## Summaries

Summaries are written by the [Claude Code](https://claude.com/claude-code)
command-line tool, using the login it already has, so there is no API key to
set up. If `claude` is installed and signed in, summaries start on their own;
if not, everything else works and sessions show their first prompt instead.

What is sent for each session: your prompts, the agent's replies, and the
names and targets of the tools it used. Tool output and file contents are not
sent. Summaries count towards your Claude plan; Haiku is used by default, and
Settings → Summaries can change the model or turn automatic summaries off.

## Command line

The same index is available from a terminal:

```sh
scrollback list                  # recent sessions
scrollback search token refresh  # fuzzy search
scrollback show runyard redesign # a session's summary
scrollback summarize -n 10       # summarise the ten newest without one
scrollback --help                # everything else
```

On a Mac the command is built at `target/release/scrollback`; the Linux
packages install it on your path.

## Where things are kept

| | macOS | Linux | Windows |
| --- | --- | --- | --- |
| Index and settings | `~/Library/Application Support/scrollback` | `~/.local/share/scrollback` | `%LOCALAPPDATA%\scrollback` |

Deleting that folder resets Scrollback; it rebuilds the index on its next
start and writes the summaries again.

## Development

```sh
cargo run --release -p scrollback-app   # the app
cargo test                              # the tests
scripts/check-platforms.sh              # Linux tests and a Windows build, in Docker
scripts/linux-preview.sh                # a picture of the app running on Linux
```

The code is in three crates: `crates/core` reads the sources, keeps the index
and writes summaries; `crates/app` is the GPUI app; `crates/cli` is the
command line.

## License

MIT. See [LICENSE](LICENSE).
