import asyncio
import importlib
import importlib.util
import io
import json
import os
import re
import sys
import threading
import time
import traceback
import urllib.request
import zipfile

import runner

SITE = None
NAME_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")
state = {"server": None, "loop": None, "task": None, "bridge": None, "thread": None, "emit": None}


def emit(kind, **data):
    callback = state["emit"]
    if callback is not None:
        callback.accept(json.dumps({"t": kind, **data}, ensure_ascii=False))


def _canon(name):
    return re.sub(r"[-_.]+", "-", name).lower()


def _fetch(url, timeout=60):
    request = urllib.request.Request(url, headers={"User-Agent": "NiosApps-Android"})
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return response.read()


def _pick_wheel(meta):
    for entry in meta["urls"]:
        if entry["packagetype"] == "bdist_wheel" and entry["filename"].endswith("-none-any.whl"):
            return entry
    return None


def _requirements(meta):
    out = []
    for line in meta["info"].get("requires_dist") or []:
        if ";" in line and "extra" in line.split(";", 1)[1]:
            continue
        head = line.split(";", 1)[0].strip()
        match = re.match(r"^([A-Za-z0-9][A-Za-z0-9._-]*)", head)
        if match:
            marker = line.split(";", 1)[1] if ";" in line else ""
            if "sys_platform" in marker and "win32" in marker and "!=" not in marker:
                continue
            out.append(match.group(1))
    return out


def install_pure(package, seen=None):
    seen = seen if seen is not None else set()
    key = _canon(package)
    if key in seen or key in BUNDLED:
        return
    seen.add(key)
    if not NAME_RE.match(package):
        raise RuntimeError(f"bad package name {package}")
    meta = json.loads(_fetch(f"https://pypi.org/pypi/{package}/json"))
    wheel = _pick_wheel(meta)
    if wheel is None:
        raise RuntimeError(f"{package}: нет чисто-питоновской сборки, на телефоне такая библиотека не работает")
    emit("log", message=f"Скачиваю {package} {meta['info']['version']}")
    data = _fetch(wheel["url"], 180)
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        archive.extractall(SITE)
    importlib.invalidate_caches()
    for dep in _requirements(meta):
        install_pure(dep, seen)


BUNDLED = {_canon(n) for n in (
    "fastapi", "pydantic", "uvicorn", "websockets", "h11", "click", "anyio", "sniffio", "idna", "starlette",
    "typing-extensions", "python-multipart", "nios-apps", "nios", "pip", "setuptools")}


def install_many(packages):
    failed = []
    if packages:
        emit("install", packages=packages)
    for package in packages:
        try:
            install_pure(package)
        except Exception as exc:
            failed.append(package)
            emit("log", message=f"{package}: {exc}")
    return failed


def _fail(code, message, detail=""):
    emit("error", code=code, message=message, detail=detail)
    stop()


def _load(source, extra):
    path = os.path.join(SITE, "user_app.py")
    with open(path, "w", encoding="utf-8") as handle:
        handle.write(source)
    tried = set()
    for _ in range(runner.MAX_AUTO_PACKAGES):
        sys.modules.pop("user_app", None)
        spec = importlib.util.spec_from_file_location("user_app", path)
        module = importlib.util.module_from_spec(spec)
        sys.modules["user_app"] = module
        try:
            spec.loader.exec_module(module)
            return module
        except SyntaxError as exc:
            _fail("syntax", f"В коде ошибка на строке {exc.lineno}: {exc.msg}.", traceback.format_exc())
            return None
        except ModuleNotFoundError as exc:
            top = (exc.name or "").split(".")[0]
            package = runner.package_for(top)
            if not top or top in tried or not runner.NAME.match(top):
                _fail("module", f"Не хватает библиотеки «{exc.name}».", traceback.format_exc())
                return None
            tried.add(top)
            if install_many([package]):
                _fail("install", f"Не получилось поставить библиотеку «{package}». На телефоне работают только библиотеки на чистом Python.", traceback.format_exc())
                return None
        except Exception as exc:
            _fail("exception", f"Код не запустился: {exc.__class__.__name__}: {exc}", traceback.format_exc())
            return None
    _fail("module", "Слишком много недостающих библиотек. Проверьте код.")
    return None


def _serve(source, key, packages, port, server_url):
    import nios
    import uvicorn

    try:
        extra = [p for p in (packages or "").split() if runner.NAME.match(p.replace("-", "_"))]
        wanted = extra + runner.plan_install(source, local=frozenset())
        missing = install_many([p for p in dict.fromkeys(wanted)])
        if missing:
            emit("log", message="Не удалось установить: " + ", ".join(missing))
        module = _load(source, extra)
        if module is None:
            return
        app = getattr(module, "app", None)
        if app is None or not callable(app):
            _fail("no_app", "В коде нет переменной app. Добавьте строку app = FastAPI().")
            return
        chosen = int(port) if str(port or "").isdigit() and 1024 <= int(port) <= 65535 else runner.free_port()
        config = uvicorn.Config(app, host="127.0.0.1", port=chosen, log_level="info", use_colors=False)
        server = uvicorn.Server(config)
        server.install_signal_handlers = lambda: None
        state["server"] = server
        threading.Thread(target=server.run, daemon=True, name="nios-uvicorn").start()
        deadline = time.time() + 20
        while time.time() < deadline and not server.started:
            time.sleep(0.1)
        if not server.started:
            _fail("server", "Сервер не смог стартовать.")
            return
        emit("local", port=chosen)

        class Bridge(nios.App):
            def _log(self, *parts):
                emit("log", message=" ".join(str(p) for p in parts))

        bridge = Bridge(key, port=chosen, server=server_url or nios.DEFAULT_SERVER)
        state["bridge"] = bridge
        loop = asyncio.new_event_loop()
        state["loop"] = loop
        asyncio.set_event_loop(loop)
        task = loop.create_task(bridge._run())
        state["task"] = task

        def announce():
            announced = None
            while state["bridge"] is bridge:
                if bridge.url and bridge.url != announced:
                    announced = bridge.url
                    emit("online", url=bridge.url)
                time.sleep(0.3)

        threading.Thread(target=announce, daemon=True).start()
        try:
            loop.run_until_complete(task)
        except asyncio.CancelledError:
            pass
        except nios.NiosError as exc:
            _fail("key", str(exc) or "Ключ не подошёл.")
        except Exception as exc:
            _fail("tunnel", f"Не удалось подключиться к Nios Apps: {exc.__class__.__name__}")
    except Exception as exc:
        _fail("runner", f"Внутренняя ошибка: {exc.__class__.__name__}: {exc}", traceback.format_exc())


def start(data_dir, source, key, packages, port, server_url, callback):
    global SITE
    stop()
    SITE = os.path.join(data_dir, "site")
    os.makedirs(SITE, exist_ok=True)
    if SITE not in sys.path:
        sys.path.insert(0, SITE)
    state["emit"] = callback
    thread = threading.Thread(target=_serve, args=(source, key, packages, port, server_url), daemon=True, name="nios-run")
    state["thread"] = thread
    thread.start()


def stop():
    bridge, server, loop, task = state["bridge"], state["server"], state["loop"], state["task"]
    state["bridge"] = state["server"] = state["task"] = None
    if bridge is not None:
        bridge.stop()
    if loop is not None and task is not None:
        loop.call_soon_threadsafe(task.cancel)
    if server is not None:
        server.should_exit = True
    emit("stopped")
