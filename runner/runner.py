import ast
import asyncio
import importlib.util
import json
import os
import re
import socket
import subprocess
import sys
import threading
import time
import traceback

IMPORT_TO_PACKAGE = {
    "PIL": "pillow", "cv2": "opencv-python", "yaml": "pyyaml", "sklearn": "scikit-learn", "bs4": "beautifulsoup4",
    "dotenv": "python-dotenv", "jwt": "pyjwt", "dateutil": "python-dateutil", "jose": "python-jose",
    "multipart": "python-multipart", "magic": "python-magic", "serial": "pyserial", "Crypto": "pycryptodome",
    "OpenSSL": "pyopenssl", "attr": "attrs", "git": "gitpython", "google": "protobuf", "psycopg2": "psycopg2-binary",
    "MySQLdb": "mysqlclient", "skimage": "scikit-image", "usb": "pyusb", "zmq": "pyzmq", "markdown": "markdown",
    "docx": "python-docx", "pptx": "python-pptx", "fitz": "pymupdf", "telegram": "python-telegram-bot", "discord": "discord.py",
    "dns": "dnspython", "ldap": "python-ldap", "slugify": "python-slugify", "socks": "pysocks", "win32api": "pywin32",
    "win32con": "pywin32", "win32com": "pywin32", "pythoncom": "pywin32", "tomli_w": "tomli-w", "ruamel": "ruamel.yaml",
    "engineio": "python-engineio", "socketio": "python-socketio", "jinja2": "jinja2", "flask_cors": "flask-cors",
    "sqlalchemy": "sqlalchemy", "pydantic_settings": "pydantic-settings", "starlette": "starlette",
}
PROVIDED = {"fastapi", "uvicorn", "nios", "pip", "setuptools", "pkg_resources", "_distutils_hack"}
MAX_AUTO_PACKAGES = 20
NAME = re.compile(r"^[A-Za-z][A-Za-z0-9_]*$")
HIDE = 0x08000000 if sys.platform == "win32" else 0
FAILED = set()


def emit(kind, **data):
    print(json.dumps({"t": kind, **data}, ensure_ascii=False), flush=True)


def fail(code, message, detail=""):
    emit("error", code=code, message=message, detail=detail)
    sys.stdout.flush()
    os._exit(1)


def watch_parent():
    try:
        sys.stdin.buffer.read()
    finally:
        os._exit(0)


def free_port():
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def imported_names(source):
    try:
        tree = ast.parse(source)
    except SyntaxError:
        return set()
    names = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            names.update(alias.name.split(".")[0] for alias in node.names)
        elif isinstance(node, ast.ImportFrom) and node.level == 0 and node.module:
            names.add(node.module.split(".")[0])
    return names


def local_names(*folders):
    found = set()
    for folder in folders:
        if not folder or not os.path.isdir(folder):
            continue
        for entry in os.listdir(folder):
            stem, ext = os.path.splitext(entry)
            if ext == ".py" or os.path.isdir(os.path.join(folder, entry)):
                found.add(stem if ext == ".py" else entry)
    return found


def package_for(import_name):
    return IMPORT_TO_PACKAGE.get(import_name, import_name)


def plan_install(source, local=frozenset(), find_spec=importlib.util.find_spec, stdlib=None):
    stdlib = stdlib if stdlib is not None else set(sys.stdlib_module_names) | set(sys.builtin_module_names)
    wanted = []
    for name in sorted(imported_names(source)):
        if name in stdlib or name in local or name in PROVIDED or name.startswith("_") or not NAME.match(name):
            continue
        try:
            present = find_spec(name) is not None
        except (ImportError, ValueError):
            present = False
        package = package_for(name)
        if not present and package not in wanted:
            wanted.append(package)
    return wanted[:MAX_AUTO_PACKAGES]


def pip_install(packages):
    command = [sys.executable, "-m", "pip", "install", "-q", "--no-warn-script-location", "--disable-pip-version-check", *packages]
    process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, creationflags=HIDE)
    for line in process.stdout:
        line = line.rstrip()
        if line:
            emit("log", message=line)
    return process.wait() == 0


def install_packages(packages):
    if not packages:
        return []
    emit("install", packages=packages)
    importlib.invalidate_caches()
    if pip_install(packages):
        return []
    failed = []
    for package in packages:
        if not pip_install([package]):
            failed.append(package)
    FAILED.update(failed)
    importlib.invalidate_caches()
    return failed


def load_user_code(path, local):
    sys.path.insert(0, os.path.dirname(os.path.abspath(path)))
    tried = set()
    for _ in range(MAX_AUTO_PACKAGES):
        spec = importlib.util.spec_from_file_location("user_app", path)
        module = importlib.util.module_from_spec(spec)
        sys.modules["user_app"] = module
        try:
            spec.loader.exec_module(module)
            return module
        except SyntaxError as exc:
            fail("syntax", f"В коде ошибка на строке {exc.lineno}: {exc.msg}.", traceback.format_exc())
        except ModuleNotFoundError as exc:
            top = (exc.name or "").split(".")[0]
            package = package_for(top)
            if not top or top in local or top in tried or not NAME.match(top):
                fail("module", f"Не хватает библиотеки «{exc.name}». Если она называется по-другому, впишите её название в настройках "
                               "в поле «Дополнительные библиотеки» и запустите снова.", traceback.format_exc())
            tried.add(top)
            failed = [package] if package in FAILED else install_packages([package])
            if failed:
                fail("install", f"Не получилось установить библиотеку «{package}». Проверьте название и интернет. Если это ваш файл, "
                                "выберите папку проекта в настройках.", traceback.format_exc())
            sys.modules.pop("user_app", None)
        except Exception as exc:
            fail("exception", f"Код не запустился: {exc.__class__.__name__}: {exc}", traceback.format_exc())
    fail("module", "Слишком много недостающих библиотек. Проверьте код.")


def start_server(app, port):
    import uvicorn

    config = uvicorn.Config(app, host="127.0.0.1", port=port, log_level="info", use_colors=False)
    server = uvicorn.Server(config)
    server.install_signal_handlers = lambda: None
    thread = threading.Thread(target=server.run, daemon=True)
    thread.start()
    deadline = time.time() + 20
    while time.time() < deadline:
        if server.started:
            return server
        if not thread.is_alive():
            fail("server", "Сервер не смог стартовать. Подробности ниже в журнале.")
        time.sleep(0.1)
    fail("server", "Сервер слишком долго запускается.")


def choose_port():
    raw = os.environ.get("NIOS_LOCAL_PORT", "").strip()
    if raw.isdigit() and 1024 <= int(raw) <= 65535:
        with socket.socket() as probe:
            try:
                probe.bind(("127.0.0.1", int(raw)))
            except OSError:
                fail("port", f"Порт {raw} уже занят другой программой. Выберите другой в настройках или очистите поле.")
        return int(raw)
    return free_port()


def main():
    key = os.environ.get("NIOS_APP_KEY", "")
    path = sys.argv[1]
    threading.Thread(target=watch_parent, daemon=True).start()
    project = os.environ.get("NIOS_PROJECT_DIR", "").strip()
    if project:
        if not os.path.isdir(project):
            fail("folder", f"Папка проекта не найдена: {project}. Выберите её заново в настройках.")
        os.chdir(project)
        sys.path.insert(0, project)
    local = local_names(project, os.path.dirname(os.path.abspath(path)))
    with open(path, encoding="utf-8") as handle:
        source = handle.read()
    missing = install_packages(plan_install(source, local))
    if missing:
        emit("log", message="Не удалось установить: " + ", ".join(missing))
    module = load_user_code(path, local)
    app = getattr(module, "app", None)
    if app is None or not callable(app):
        fail("no_app", "В коде нет переменной app. Добавьте строку app = FastAPI().")
    port = choose_port()
    start_server(app, port)
    emit("local", port=port)

    import nios

    class Bridge(nios.App):
        def _log(self, *parts):
            emit("log", message=" ".join(str(p) for p in parts))

    bridge = Bridge(key, port=port, server=os.environ.get("NIOS_SERVER") or nios.DEFAULT_SERVER)

    def tunnel():
        try:
            asyncio.run(bridge._run())
        except nios.NiosError as exc:
            fail("key", str(exc) or "Ключ не подошёл.")
        except Exception as exc:
            fail("tunnel", f"Не удалось подключиться к Nios Apps: {exc.__class__.__name__}")

    threading.Thread(target=tunnel, daemon=True).start()
    announced = None
    while True:
        if bridge.url and bridge.url != announced:
            announced = bridge.url
            emit("online", url=bridge.url)
        time.sleep(0.3)


if __name__ == "__main__":
    try:
        main()
    except SystemExit:
        raise
    except Exception as exc:
        emit("error", code="runner", message=f"Внутренняя ошибка: {exc.__class__.__name__}: {exc}", detail=traceback.format_exc())
        sys.exit(1)
