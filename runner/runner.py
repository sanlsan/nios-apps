import asyncio
import importlib.util
import json
import os
import socket
import sys
import threading
import time
import traceback


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


def load_user_code(path):
    sys.path.insert(0, os.path.dirname(os.path.abspath(path)))
    spec = importlib.util.spec_from_file_location("user_app", path)
    module = importlib.util.module_from_spec(spec)
    sys.modules["user_app"] = module
    try:
        spec.loader.exec_module(module)
    except SyntaxError as exc:
        fail("syntax", f"В коде ошибка на строке {exc.lineno}: {exc.msg}.", traceback.format_exc())
    except ModuleNotFoundError as exc:
        fail("module", f"Не хватает библиотеки «{exc.name}». Впишите её в поле «Дополнительные библиотеки» и запустите снова.",
             traceback.format_exc())
    except Exception as exc:
        fail("exception", f"Код не запустился: {exc.__class__.__name__}: {exc}", traceback.format_exc())
    return module


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


def main():
    key = os.environ.get("NIOS_APP_KEY", "")
    path = sys.argv[1]
    threading.Thread(target=watch_parent, daemon=True).start()
    module = load_user_code(path)
    app = getattr(module, "app", None)
    if app is None or not callable(app):
        fail("no_app", "В коде нет переменной app. Добавьте строку app = FastAPI().")
    port = free_port()
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
