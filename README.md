# ClumsyLoader

**ClumsyLoader** is a terminal-based Rust application for downloading backups from any Pterodactyl-based panel (Bloom, Lagless, Pelican, self-hosted, ...) using the client API.

It features a simple interactive TUI (terminal user interface) to:
- Select a server from your account.
- Choose from the list of available backups.
- Download the selected backup to your local machine.

## Why use ClumsyLoader?

Bloom's scheduled backups automatically delete the oldest backup once the limit is reached. ClumsyLoader gives you the ability to automatically grab and store that backup elsewhere before it's deleted — useful for external backups and archival.

## How to Use

**Run the binary** which can be obtained from the [releases](https://github.com/ClumsyAdmin/ClumsyLoader/releases) tab


When prompted:
Paste your client API key (create one under Account > API Credentials; it starts with `ptlc_`).

Enter your panel URL, e.g. `panel.lagless.gg` (or press Enter to use the default mc.bloom.host). Pasting a full browser URL also works.

Use the arrow keys to navigate the server and backup selection lists, then press Enter to confirm.

Your selected backup will be downloaded in your working directory.

You can skip the prompts by setting the `PANEL_API_KEY` and `PANEL_URL` environment variables.

ClumsyLoader uses its own TLS implementation (rustls), so it also works with panels that require TLS 1.3 on Windows 10.
