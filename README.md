# Nios Apps for Windows

Put a local server on the internet at `ni-os.ru/apps/<name>`: paste your app key, paste your FastAPI code, press **Запустить** (Run).

Русский: окно с двумя полями. В первое вставляете ключ приложения из кабинета [ni-os.ru/appsdev](https://ni-os.ru/appsdev), во второе код сервера на FastAPI, нажимаете «Запустить» и получаете публичный адрес. Python и библиотеки программа скачивает сама.

## Download

Get `NiosApps.exe` from [Releases](../../releases) and check it against `NiosApps.exe.sha256`:

```powershell
(Get-FileHash NiosApps.exe -Algorithm SHA256).Hash.ToLower()
```

The file is not code-signed, so Windows SmartScreen may warn: choose **More info**, then **Run anyway**. Every release is built from a tagged commit by GitHub Actions, so what you download is what is in this repository.

## What it does

1. On first start it downloads the official Python 3.11 embeddable build from python.org and installs `fastapi`, `uvicorn` and [`nios-apps`](https://pypi.org/project/nios-apps/) from PyPI into `%LOCALAPPDATA%\NiosApps` (about a minute, once).
2. It saves your code to `%LOCALAPPDATA%\NiosApps\user\main.py`, installs every third-party library the code imports (the import name is mapped to the PyPI name, for example `PIL` to `pillow`, `yaml` to `pyyaml`; a missing library found at run time is installed and the code is retried) and starts it with uvicorn on a free local port.
3. `runner/runner.py` connects that port to Nios Apps with the `nios` client, so requests to your public address reach your PC.

Settings (the gear icon): a project folder the code runs from (for your own modules, templates, databases and files), a fixed local port, extra libraries to install by hand, open the data folder, reinstall Python.

Your code runs on your own computer with your own permissions. Run only code you trust. Libraries are installed from PyPI by import name, so check unfamiliar imports in code you did not write.

## Requirements

Windows 10 (1803+) or 11, Microsoft Edge WebView2 (preinstalled on current Windows), internet access.

## Build

Use the GitHub Actions workflow, or locally:

```
rustup default stable
cargo build --release
```

The MSVC toolchain (Visual Studio Build Tools, "Desktop development with C++") is required on Windows. `build.bat` installs everything and builds `dist\NiosApps.exe`.

Run without a window (also works on Linux for development):

```
NiosApps --headless --key nios_app_... --file main.py [--packages "requests pydantic"] [--folder DIR] [--port 8123]
```

Tests: `cargo test --lib`.

## Layout

- `src/lib.rs` runtime setup, settings, events, process management
- `src/gui.rs` window (wry/tao, WebView2)
- `runner/runner.py` runs user code and the tunnel
- `ui/index.html` interface

## Releasing (maintainers)

GitHub builds and publishes everything, no local tools needed. Either push a tag (`git tag v0.1.1 && git push origin v0.1.1`) or open **Actions, release, Run workflow** and type the version. The `release` workflow builds `NiosApps.exe` on Windows and attaches it with its SHA-256 to a GitHub release.

A version with a dash (`v0.2.0-rc1`) is a pre-release. [ni-os.ru/appsdev](https://ni-os.ru/appsdev) links to the latest regular release only, so test a pre-release first and publish a plain `v0.2.0` when it works.

License: MIT. See `NOTICE` for fonts.
