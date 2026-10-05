import asyncio
import base64
import http.client
import json
import queue
import struct
import threading
from concurrent.futures import ThreadPoolExecutor

import websockets

__all__ = ["app", "polling", "App", "NiosError"]
__version__ = "0.3.0"

DEFAULT_SERVER = "wss://ni-os.ru/apps/_/connect"

T_OPEN, T_DATA, T_CLOSE, T_UDP, T_WSOPEN, T_WSTEXT, T_WSBIN, T_WSCLOSE = range(1, 9)
T_HREQ, T_HBODY, T_HEND, T_HRES, T_HDATA, T_HDONE, T_HERR, T_HACK, T_HABORT = range(9, 18)
WINDOW = 4 * 1024 * 1024
PIECE = 256 * 1024
HEADER = struct.Struct(">BI")
FRAME_LIMIT = 64 * 1024 * 1024
LOOPBACK = {"127.0.0.1", "localhost", "::1"}
CHUNK = 16384

_default = None


class NiosError(Exception):
    pass


def pack(kind, sid, payload=b""):
    return HEADER.pack(kind, sid) + payload


class App:
    def __init__(self, key, port=8000, tcp=None, udp=None, host="127.0.0.1",
                 server=DEFAULT_SERVER, workers=16, quiet=False):
        if not isinstance(key, str) or not key.startswith("nios_app_"):
            raise NiosError("API key must start with nios_app_")
        if host not in LOOPBACK:
            raise NiosError("only localhost targets are allowed")
        self.key = key
        self.port = int(port) if port else None
        self.tcp = int(tcp) if tcp else None
        self.udp = int(udp) if udp else None
        self.host = host
        self.server = server
        self.quiet = quiet
        self.url = None
        self.tcp_address = None
        self.udp_address = None
        self._pool = ThreadPoolExecutor(max_workers=workers)
        self._stop = threading.Event()
        self._streams = {}
        self._http = {}
        self._ws = None

    def _log(self, *parts):
        if not self.quiet:
            print("[nios]", *parts, flush=True)

    def _forward(self, msg):
        path = msg.get("path") or "/"
        if msg.get("query"):
            path += "?" + msg["query"]
        headers = dict(msg.get("headers") or {})
        headers["Host"] = f"{self.host}:{self.port}"
        headers["Accept-Encoding"] = "identity"
        body = base64.b64decode(msg.get("body") or "")
        conn = http.client.HTTPConnection(self.host, self.port, timeout=25)
        try:
            conn.request(msg["method"], path, body=body or None, headers=headers)
            res = conn.getresponse()
            limit = int(msg.get("max") or 0)
            data = res.read(limit + 1) if limit else res.read()
            if limit and len(data) > limit:
                return {"t": "err", "id": msg["id"], "msg": "response exceeds the remaining traffic budget"}
            out = {}
            for name, value in res.getheaders():
                out[name] = f"{out[name]}, {value}" if name in out else value
            return {"t": "res", "id": msg["id"], "status": res.status, "headers": out,
                    "body": base64.b64encode(data).decode("ascii")}
        except OSError as exc:
            return {"t": "err", "id": msg["id"], "msg": f"localhost:{self.port} unreachable ({exc.__class__.__name__})"}
        finally:
            conn.close()

    def _http_send(self, loop, kind, sid, payload=b""):
        return asyncio.run_coroutine_threadsafe(self._send(kind, sid, payload), loop).result(60)

    def _http_stream(self, loop, sid, head, state):
        conn = None
        try:
            path = head.get("path") or "/"
            if head.get("query"):
                path += "?" + head["query"]
            headers = dict(head.get("headers") or {})
            headers["Host"] = f"{self.host}:{self.port}"
            headers["Accept-Encoding"] = "identity"
            length = head.get("length")
            if length is not None:
                headers["Content-Length"] = str(length)

            def body():
                while True:
                    try:
                        kind, data = state["inbox"].get(timeout=300)
                    except queue.Empty:
                        raise OSError("upload stalled")
                    if kind == T_HABORT:
                        state["abort"].set()
                        raise OSError("aborted")
                    if kind == T_HEND:
                        return
                    self._http_send(loop, T_HACK, sid, len(data).to_bytes(4, "big"))
                    yield data

            conn = http.client.HTTPConnection(self.host, self.port, timeout=300)
            has_body = head["method"] not in ("GET", "HEAD") or bool(length)
            if has_body:
                if length is None:
                    headers["Transfer-Encoding"] = "chunked"
                conn.request(head["method"], path, body=body(), headers=headers, encode_chunked=length is None)
            else:
                conn.request(head["method"], path, headers=headers)
            res = conn.getresponse()
            out = {}
            for name, value in res.getheaders():
                out[name] = f"{out[name]}, {value}" if name in out else value
            self._http_send(loop, T_HRES, sid, json.dumps({"status": res.status, "headers": out}).encode())
            while not state["abort"].is_set():
                data = res.read(PIECE)
                if not data:
                    break
                with state["cond"]:
                    while state["sent"] - state["acked"] >= WINDOW and not state["abort"].is_set():
                        state["cond"].wait(1)
                if state["abort"].is_set():
                    break
                state["sent"] += len(data)
                self._http_send(loop, T_HDATA, sid, data)
            if not state["abort"].is_set():
                self._http_send(loop, T_HDONE, sid)
        except Exception as exc:
            if not state["abort"].is_set():
                try:
                    self._http_send(loop, T_HERR, sid, f"localhost:{self.port} {exc.__class__.__name__}".encode())
                except Exception:
                    pass
        finally:
            self._http.pop(sid, None)
            if conn is not None:
                conn.close()

    def _http_frame(self, kind, sid, payload):
        state = self._http.get(sid)
        if state is None:
            return
        if kind == T_HACK and len(payload) == 4:
            with state["cond"]:
                state["acked"] += int.from_bytes(payload, "big")
                state["cond"].notify_all()
        else:
            if kind == T_HABORT:
                state["abort"].set()
                with state["cond"]:
                    state["cond"].notify_all()
            state["inbox"].put((kind, payload))

    async def _send(self, kind, sid, payload=b""):
        if self._ws is not None:
            await self._ws.send(pack(kind, sid, payload))

    async def _tcp_open(self, sid, queue):
        if not self.tcp:
            self._streams.pop(sid, None)
            await self._send(T_CLOSE, sid)
            return
        try:
            reader, writer = await asyncio.open_connection(self.host, self.tcp)
        except OSError:
            self._streams.pop(sid, None)
            await self._send(T_CLOSE, sid)
            return

        async def pump_in():
            while True:
                data = await reader.read(CHUNK)
                if not data:
                    return
                await self._send(T_DATA, sid, data)

        async def pump_out():
            while True:
                kind, payload = await queue.get()
                if kind != T_DATA:
                    return
                writer.write(payload)
                await writer.drain()

        tasks = [asyncio.ensure_future(pump_in()), asyncio.ensure_future(pump_out())]
        try:
            await asyncio.wait(tasks, return_when=asyncio.FIRST_COMPLETED)
        finally:
            for task in tasks:
                task.cancel()
            self._streams.pop(sid, None)
            writer.close()
            await self._send(T_CLOSE, sid)

    async def _udp_peer(self, sid, first):
        if not self.udp:
            self._streams.pop(sid, None)
            return
        loop = asyncio.get_running_loop()
        queue = self._streams[sid]
        outer = self

        class Relay(asyncio.DatagramProtocol):
            def datagram_received(self, data, addr):
                asyncio.ensure_future(outer._send(T_UDP, sid, data))

        transport, _ = await loop.create_datagram_endpoint(Relay, remote_addr=(self.host, self.udp))
        try:
            transport.sendto(first)
            while True:
                kind, payload = await queue.get()
                if kind != T_UDP:
                    return
                transport.sendto(payload)
        finally:
            transport.close()
            self._streams.pop(sid, None)

    async def _ws_open(self, sid, info):
        if not self.port:
            await self._send(T_WSOPEN, sid, b'{"ok":false}')
            return
        uri = f"ws://{self.host}:{self.port}{info.get('path') or '/'}"
        if info.get("query"):
            uri += "?" + info["query"]
        try:
            local = await websockets.connect(uri, subprotocols=info.get("subprotocols") or None,
                                             max_size=1024 * 1024, open_timeout=8)
        except Exception:
            await self._send(T_WSOPEN, sid, b'{"ok":false}')
            return
        queue = asyncio.Queue()
        self._streams[sid] = queue
        await self._send(T_WSOPEN, sid, json.dumps({"ok": True, "subprotocol": local.subprotocol}).encode())

        async def pump_in():
            async for message in local:
                if isinstance(message, str):
                    await self._send(T_WSTEXT, sid, message.encode())
                else:
                    await self._send(T_WSBIN, sid, message)

        async def pump_out():
            while True:
                kind, payload = await queue.get()
                if kind == T_WSTEXT:
                    await local.send(payload.decode("utf-8", "replace"))
                elif kind == T_WSBIN:
                    await local.send(payload)
                else:
                    return

        tasks = [asyncio.ensure_future(pump_in()), asyncio.ensure_future(pump_out())]
        try:
            await asyncio.wait(tasks, return_when=asyncio.FIRST_COMPLETED)
        finally:
            for task in tasks:
                task.cancel()
            self._streams.pop(sid, None)
            await local.close()
            await self._send(T_WSCLOSE, sid)

    def _binary(self, raw):
        kind, sid = HEADER.unpack_from(raw)
        payload = raw[HEADER.size:]
        if kind == T_HREQ:
            state = {"inbox": queue.Queue(), "cond": threading.Condition(), "sent": 0, "acked": 0, "abort": threading.Event()}
            self._http[sid] = state
            self._pool.submit(self._http_stream, asyncio.get_running_loop(), sid, json.loads(payload), state)
            return
        if kind in (T_HBODY, T_HEND, T_HACK, T_HABORT):
            self._http_frame(kind, sid, payload)
            return
        if kind == T_OPEN:
            q = self._streams[sid] = asyncio.Queue()
            asyncio.ensure_future(self._tcp_open(sid, q))
        elif kind == T_WSOPEN:
            asyncio.ensure_future(self._ws_open(sid, json.loads(payload)))
        elif kind == T_UDP and sid not in self._streams:
            self._streams[sid] = asyncio.Queue()
            asyncio.ensure_future(self._udp_peer(sid, payload))
        else:
            q = self._streams.get(sid)
            if q is not None:
                if kind in (T_CLOSE, T_WSCLOSE):
                    q.put_nowait((T_CLOSE, b""))
                else:
                    q.put_nowait((kind, payload))

    async def _session(self):
        async with websockets.connect(self.server, max_size=FRAME_LIMIT,
                                      ping_interval=20, ping_timeout=30) as ws:
            self._ws = ws
            await ws.send(json.dumps({"t": "auth", "key": self.key, "cap": {"frame": FRAME_LIMIT, "http_stream": 1}}))
            loop = asyncio.get_running_loop()
            try:
                async for raw in ws:
                    if isinstance(raw, bytes):
                        self._binary(raw)
                        continue
                    msg = json.loads(raw)
                    kind = msg.get("t")
                    if kind == "hello":
                        self.url = msg["url"]
                        self.tcp_address = msg.get("tcp")
                        self.udp_address = msg.get("udp")
                        self._log(f"online: {self.url}")
                        if self.tcp_address:
                            self._log(f"tcp: {self.tcp_address['host']}:{self.tcp_address['port']}")
                        if self.udp_address:
                            self._log(f"udp: {self.udp_address['host']}:{self.udp_address['port']}")
                    elif kind == "error":
                        raise NiosError(msg.get("msg", "connection refused"))
                    elif kind == "req":
                        loop.create_task(self._serve(ws, msg))
            finally:
                self._ws = None
                for q in list(self._streams.values()):
                    q.put_nowait((T_CLOSE, b""))
                for state in list(self._http.values()):
                    state["abort"].set()
                    state["inbox"].put((T_HABORT, b""))

    async def _serve(self, ws, msg):
        loop = asyncio.get_running_loop()
        reply = await loop.run_in_executor(self._pool, self._forward, msg)
        await ws.send(json.dumps(reply))

    async def _run(self):
        delay = 1.0
        while not self._stop.is_set():
            try:
                await self._session()
                delay = 1.0
            except NiosError as exc:
                self._log("refused:", exc)
                raise
            except websockets.exceptions.ConnectionClosed as exc:
                if exc.rcvd and exc.rcvd.code in (4401, 4404):
                    raise NiosError(exc.rcvd.reason or "key is invalid or the app was deleted") from None
                self._log("connection closed, reconnecting")
            except (OSError, websockets.exceptions.WebSocketException) as exc:
                self._log(f"no connection ({exc.__class__.__name__}), retry in {delay:.0f}s")
            await asyncio.sleep(delay)
            delay = min(delay * 2, 30.0)

    def polling(self, background=False):
        if background:
            thread = threading.Thread(target=lambda: asyncio.run(self._run()), daemon=True, name="nios-polling")
            thread.start()
            return thread
        try:
            asyncio.run(self._run())
        except KeyboardInterrupt:
            self._log("stopped")

    def stop(self):
        self._stop.set()


def app(key, port=8000, **options):
    global _default
    _default = App(key, port, **options)
    return _default


def polling(background=False):
    if _default is None:
        raise NiosError("call nios.app(key, port=...) first")
    return _default.polling(background)
