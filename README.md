# 🖨️ MyStampa (Rust)

**Automated thermal print service for the MySagra ecosystem**, rewritten in Rust.

Same behavior as the Node.js MyStampa: it receives orders from the backend via Server-Sent Events and prints them on ESC/POS network printers. It runs in two ways:

- **Docker**, alongside the rest of the MySagra stack (configured with `API_URL` / `API_KEY`, as before)
- **Standalone executable** for Windows and Linux: a single file, nothing to install, asks for the API link and token at startup

---

## Features

- **SSE events**: `confirmed-order`, `reprint-order`, `order-cancelled`, `general-closure`, `open-drawer`, `settings-update`; reconnects forever with exponential backoff and `Last-Event-ID`
- **Kitchen receipts** routed per food printer, with a progressive number per printer
- **Cash receipt** with prices, extras, discount, payment method, sagra logo (from `sagra.receiptLogo`, MySagra logo as fallback) and the MySagra footer
- **Single tickets** (`food.separateTicket`) and **pickup tickets per station** (`settings.stations.pickupTickets`)
- **Cancellation receipts** and **general closure report** (main report + one ticket per category)
- **Cash drawer** opening (on cash payments and on `open-drawer`)
- **Paper check** before printing (`DLE EOT 4`); failed jobs are queued and retried every 60 seconds
- **Printer monitoring** every 2 minutes, PATCHing status changes to the backend; MAC → IP fallback through the ARP table (Linux `/proc/net/arp`, Windows `arp -a`)
- Settings (`showNumbers`, currency, pickup tickets, logo) applied live from `settings-update`

---

## Desktop app (Windows / Linux)

`mystampa-windows-x86_64.exe`, `MyStampa-x86_64.AppImage` or `mystampa-linux-x86_64`: a single file, nothing to install. Put it in a folder and open it with a double-click.

A window in the MySagra style opens:

1. **Connection** — enter the **link of the MySagra API** (e.g. `http://192.168.1.10:4300`) and the **API token** (`X-API-KEY`). MyStampa checks them against the backend and saves them in `mystampa.json` next to the executable. If the check fails you can still save and start (e.g. backend temporarily off).
2. **Status** — server connection, printers online, prints waiting in the queue, last order, the printers with their live status and the activity log. Buttons open the log folder or change the connection.

With a saved connection MyStampa starts printing as soon as it opens (also after a power cut). Closing the window asks for confirmation, since orders are not printed while it is closed.

From a terminal, `--console` runs it without the window (asks link and token in the terminal); without a display (a server) console mode is used automatically.

Logs are written to `logs/` next to the executable.

> If the folder of the executable is not writable, the configuration and logs go to `%APPDATA%\mystampa` (Windows) or `~/.config/mystampa` (Linux). The Linux build needs glibc 2.36 or newer (Debian 12, Ubuntu 23.04 and later).

---

## Docker

```bash
cp .env.example .env   # fill in API_URL and API_KEY
docker compose up -d
```

With `API_URL` and `API_KEY` set, MyStampa never asks anything. In the MySagra stack (`dockercompose/docker-compose.yaml`) only the build context changes, from `../mystampa` to `../mystampa-rust`: environment variables and the `/app/logs` volume are the same. MyStampa no longer serves a web page, so the `expose: "3032"` entry (and any Caddy route to it) can be removed.

---

## Configuration

| Variable    | Description                                                      | Default |
|-------------|------------------------------------------------------------------|---------|
| `API_URL`   | Base URL of the MySagra backend                                  | asked at startup |
| `API_KEY`   | API key for the print service (`X-API-KEY`)                      | asked at startup |
| `LOG_LEVEL` | `error`, `warn`, `info`, `debug`, `trace` (`http`/`verbose`/`silly` also accepted) | `info` |
| `LOG_DIR`   | Directory for the daily JSON log files (7 days kept)            | `logs/` next to the config, `/app/logs` in Docker |
| `TZ`        | Time zone of log timestamps (receipts always use Europe/Rome)    | `Europe/Rome` in Docker |

---

## Building

Requires [Rust](https://rustup.rs) for development, only Docker for the release executables.

```bash
cargo run                       # development (window)
cargo run -- --console          # development (terminal)
cargo test                      # unit tests
./scripts/build-release.sh      # dist/: Linux executable, AppImage, Windows .exe
docker compose build            # Docker image (headless, built without the GUI)
```

The window is the `gui` Cargo feature (on by default, egui/eframe); the Docker image is built with `--no-default-features`. `SKIP_APPIMAGE=1 ./scripts/build-release.sh` skips the AppImage, which downloads `appimagetool` from the AppImage project.

---

## Project structure

```
src/
├── main.rs       # window, console or Docker mode
├── gui.rs        # desktop window (connection + status), MySagra style
├── service.rs    # service startup and background tasks
├── config.rs     # API link/token: env vars, terminal prompt, mystampa.json
├── api.rs        # backend calls with retry
├── sse.rs        # SSE client and event dispatch
├── print.rs      # order, cancellation, drawer and closure handling
├── escpos.rs     # receipt builders, CP858 encoding, TCP printing
├── image.rs      # logo → ESC/POS raster
├── queue.rs      # retry queue for failed prints
├── monitor.rs    # printer list refresh and status probing
├── arp.rs        # MAC → IP resolution
├── state.rs      # shared state, settings, logo cache
└── logging.rs    # console + daily JSON files
resources/        # MySagra logo, footer, app icon, Geist font (embedded in the executable)
packaging/        # AppImage files
build.rs          # icon and version info in the Windows .exe
```

---

## License

GNU Affero General Public License v3.0 — see [LICENSE.md](LICENSE.md).

The bundled Geist font (`resources/fonts/Geist-Regular.ttf`, by Vercel) is licensed under the SIL Open Font License 1.1.
