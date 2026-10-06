<div align="center">

<img src="src-tauri/icons/128x128.png" width="96" alt="Coucou icon">

# Coucou for Windows

**Mochi doesn't get a notch on a PC — so it lives at the top of your screen instead.**

Approve Claude Code permissions, watch your session work, drop a file, chat with Claude, keep an eye on your services — without leaving what you're doing.

![Windows 10/11](https://img.shields.io/badge/Windows-10%2F11-0078D4?logo=windows)
![Tauri 2](https://img.shields.io/badge/Tauri-2-FFC131?logo=tauri&logoColor=black)
![Rust](https://img.shields.io/badge/Rust-backend-000?logo=rust)
![License: MIT](https://img.shields.io/badge/license-MIT-green)

</div>

<img src="screenshots/greeting.png" width="640" alt="Mochi waving hello at launch">

---

## Install

1. Download `Coucou-Windows-setup.exe` from the [latest release](../../releases/tag/windows-latest).
2. Run it. It installs for the current user only — no admin prompt.
3. Coucou starts, waves hello, and then gets out of the way.

### "Windows protected your PC"

The installer isn't code-signed yet, so **SmartScreen** shows a blue warning the first
few times anybody downloads it:

> Windows protected your PC — Microsoft Defender SmartScreen prevented an unrecognised app from starting.

Click **More info**, then **Run anyway**. That's it. Signing is on the list; until
then this is what an unsigned installer looks like on Windows, and you can always
[build it yourself](#build-it-yourself) if you'd rather not trust a download.

## Using it

<img src="screenshots/compact.png" width="292" alt="The compact island, with the integration pills as mini Mochis">
<img src="screenshots/overview.png" width="640" alt="The overview: the focused integration on the left, the other pills on the right">
<img src="screenshots/approval.png" width="640" alt="A Claude Code permission request, with Deny and Allow">
<img src="screenshots/chat.png" width="640" alt="Chatting with Claude from the island">
<img src="screenshots/drop.png" width="640" alt="Mochi turned into a box, waiting for a file">

| What you do | What happens |
|---|---|
| Move the mouse to the very top-centre of the screen | Mochi peeks out |
| Click the small island | It opens |
| Click Mochi | It gets annoyed. Three times in a row and it goes dizzy |
| Rest the pointer on Mochi for two seconds | Hearts |
| Drag a file onto the island | Mochi turns into a box, swallows it, then offers to answer questions about it |
| `Esc` | Closes the island |
| Tray icon | Open, Settings…, Pause, Quit |

Everything else happens on its own: a Claude Code permission request opens the
island with **Deny / Allow**, a finished session shows what it did, and
your integrations sit in the coloured pills next to Mochi.

## Local AI with Ollama

Coucou can answer with a model running **on your own PC** instead of Claude —
free, offline, and nothing leaves the machine.

1. Install [Ollama](https://ollama.com/download) and pull a model that also reads
   images: `ollama pull gemma3` (lighter: `gemma3:4b`; bigger GPU: `gemma3:12b`
   or `qwen2.5vl:7b`).
2. **Settings… → AI engine → Ollama (local)**, click **Refresh**, pick the model.
3. Drop a file on the island and choose **Correct it**, **Summarize** or ask
   anything. Answers stream in word by word.

What a local model can read: text and code, Word (`.docx`, `.odt`), PDFs (the
text layer; scanned PDFs are rendered to images with Windows' own PDF engine and
need a vision model), and images (PNG, JPEG, WebP, GIF, BMP, TIFF — shrunk to
1600 px). The **Context** setting decides how much of a long document fits.
No web search in local mode.

### Correcting

- **Drop a whole folder to correct it in one go:** every text, code and Word
  file gets a corrected copy; files that can't become a corrected file (PDF,
  images) are listed as skipped rather than answered in the chat.
- **Drop several files, or a whole folder.** Folders are walked (up to 25 files,
  4 levels deep; `node_modules`, `.git`, build output and binaries are skipped)
  and everything goes into the same conversation.
- **Correct it on a Word file** writes `<name> (corrigé).docx` next to the
  original, with every correction as a Word *tracked change* by "Mochi" — review
  them with Review → Accept / Reject. The original is never touched. Paragraphs
  a rebuild could damage (links, fields, images, comments, mixed bold/italic
  inside a sentence) are left as they are, and answers that rewrite instead of
  correcting are refused.
- **Correct the selection anywhere**: select text in any app and press
  `Ctrl+Alt+Shift+C` (changeable in Settings → AI engine). Mochi copies it,
  corrects it, pastes it back in place and restores your clipboard text.
  `Ctrl+Z` in the app undoes it. If you switched windows meanwhile, nothing is
  pasted and the correction waits on the clipboard.

Both work with Claude or with the local Ollama model.

`scripts/Installer Coucou.bat` reinstalls Coucou from `release/` and sets up
Ollama in one go.

## Throw Mochi at a window

Grab Mochi and drag it onto a browser window: Coucou reads that tab's address,
fetches what it is, and opens the chat with it attached so you can ask anything.
A **GitHub repo** comes back as its description, languages, file tree and README;
**any web page** as its readable text. Then type your prompt (a default is filled
in) and Claude or the local Ollama model analyses it — Coucou does the fetching,
so it works with an offline model too.

Reading the address bar uses Windows UI Automation and works on Chromium
browsers (Chrome, Edge, Brave, Opera GX…) and Firefox; on an unrecognised window Coucou
says it couldn't read the address rather than guessing.

## Push to GitHub

The GitHub card has a **Push a project…** button: pick a local folder, type a
commit message, and Coucou runs `git add -A`, `git commit` and `git push` to the
repo's `origin` for you. For a github.com remote it authenticates over HTTPS with
the token you stored in Settings (it never writes the token into the repo config,
and strips it from any message shown back); any other host uses your system's own
git credentials. If the folder has no `origin` yet, Coucou offers to **create the GitHub repo**
for you (public by default) from the folder name, wire it as origin, and push —
one click from an empty folder to a published repo. Errors (wrong token,
non-fast-forward, offline, name already taken) come back in plain words.
Requires Git installed and on `PATH`.

## Claude Code

<img src="screenshots/settings.png" width="562" alt="The settings window">

Open **Settings… → Claude Code → Install hooks…**. You get the exact diff of what
will change in `%USERPROFILE%\.claude\settings.json`, the path of the dated backup
that will be taken, and nothing is written until you click. Your own hooks are
never touched, and uninstalling removes only Coucou's entries.

The relay is a tiny executable, `coucou-hook.exe`, copied to
`%LOCALAPPDATA%\Coucou\bin\` at launch. It is given 300 ms to reach Coucou and
exits cleanly if the app is closed, slow or crashed — **a Claude Code session is
never blocked or slowed down by Coucou.** If nobody answers a permission request
in time, Coucou stays quiet and Claude Code asks in the terminal as usual.

It works from any terminal — Windows Terminal, PowerShell, VS Code, Git Bash.

## Chat and keys

**Settings… → Claude** takes your Anthropic API key. Keys live in the **Windows
Credential Manager**, never on disk and never in the interface — the island can
only ask whether a key exists. Same for every integration key.

No telemetry. The only network requests Coucou makes are to the services you
configure yourself.

## Build it yourself

You need [Rust](https://rustup.rs), [Node 20+](https://nodejs.org), and the
**MSVC build tools** (Visual Studio Build Tools with "Desktop development with
C++"). WebView2 ships with Windows 10/11.

```powershell
cd windows
npm install
npm run tauri dev      # live-reloading development build
npm run pack           # builds the installer and drops it in windows/release/
```

`npm run dev` alone serves the front end in an ordinary browser, which is enough
to work on the island's looks. It also serves `dev/upload-preview.html`, which
replays the whole file-drop choreography on a loop — the one part of the UI that
otherwise needs a real drag from Explorer to see. Neither page ships in the app.

`npm run pack` leaves two files in `windows/release/`, the same names the release
workflow publishes:

```
Coucou-Windows-X.Y.Z-setup.exe    the versioned installer
Coucou-Windows-setup.exe          the same file under the rolling name
```

Installing is optional — `target/release/coucou.exe` runs on its own. There is no
window in the taskbar and no console: the island at the top of the screen and the
Mochi in the notification area are the whole app, and Quit lives in its menu.

The 28 sounds are the macOS app's own files; they are never duplicated in this
folder. The path is declared once, in `SOUNDS_DIR` at the top of
`vite.config.ts` — when they move to `shared/sounds/`, change that one line.

The app icon and the tray icon are drawn in code, like Mochi itself:

```powershell
npm run icons          # regenerates src-tauri/icons from scripts/gen-icons.mjs
```

### Layout

```
windows/
  src/                 island front end (TypeScript, no framework)
    mochi/             Mochi and the launch greeting, in Canvas 2D
    island/            state machine, hooks, integrations
    views/             every island view
    settings/          the settings window
  src-tauri/           Rust backend: window, named pipe, Claude API, pollers
  hook/                coucou-hook.exe, the Claude Code relay
  scripts/             icon generator
```

### Log

`%LOCALAPPDATA%\Coucou\coucou.log` — hook events, permission decisions, poller
problems. It stays on your machine.

## What's different from the Mac version

- No notch, so the island lives at the top centre of the screen and retracts into
  the top edge instead of hiding in a notch.
- Permission approval works from **any** terminal; the Mac build only listens to
  VS Code sessions.
- Not in this version: sending a file by email, dragging Mochi onto a window to
  attach it as context, and jumping to a specific terminal window — "Open
  terminal" opens the working folder in VS Code when `code` is on your `PATH`.
- Cal.com shows the next bookings as a list rather than the Mac's calendar.
