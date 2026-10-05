import argparse
import os
import sys

from . import App, NiosError, __version__, DEFAULT_SERVER

PREFIX = "nios_app_"


def main(argv=None):
    parser = argparse.ArgumentParser(prog="nios", description="Expose a local server at ni-os.ru/apps/<name>")
    parser.add_argument("key", nargs="?", default=os.environ.get("NIOS_APP_KEY"), help="API key from ni-os.ru/appsdev")
    parser.add_argument("-p", "--port", type=int, default=8000, help="local HTTP port, 0 to disable (default 8000)")
    parser.add_argument("--tcp", type=int, help="local port for raw TCP traffic")
    parser.add_argument("--udp", type=int, help="local port for raw UDP traffic")
    parser.add_argument("--server", default=DEFAULT_SERVER, help=argparse.SUPPRESS)
    parser.add_argument("--version", action="version", version=__version__)
    args = parser.parse_args(argv)
    if not args.key:
        parser.error("API key is required: nios nios_app_XXXX --port 8000")
    key = args.key.strip()
    if not key.startswith(PREFIX):
        key = PREFIX + key
    try:
        App(key, port=args.port or None, tcp=args.tcp, udp=args.udp, server=args.server).polling()
    except NiosError as exc:
        print(f"[nios] {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
