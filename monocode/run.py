#!/usr/bin/env python3
"""Run the Phoenix frontend with this checkout's native Phoenix services."""
import argparse
import errno
import json
import importlib.util
import os
from pathlib import Path
import signal
import subprocess
import threading
from http.server import ThreadingHTTPServer
from urllib.request import urlopen

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parent


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def binary(name, directory, override):
    if override:
        path = Path(override).expanduser().resolve()
        if path.is_file():
            return path
        raise RuntimeError(f"Configured {name} executable is missing: {path}")
    for profile in ("release", "debug"):
        path = directory / "target" / profile / name
        if path.is_file():
            return path
    raise RuntimeError(f"Build {name} in {directory} before starting Phoenix.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gateway", default=os.environ.get("PHOENIX_GATEWAY_BINARY"))
    parser.add_argument("--desktop", default=os.environ.get("PHOENIX_DESKTOP_BINARY"))
    args = parser.parse_args()
    gateway = binary("phoenix", REPO, args.gateway)
    desktop = binary("phoenix-desktop", REPO / "canvas-app", args.desktop)
    if not (REPO / "canvas-app/chromium-shell/node_modules/electron/dist/electron").is_file():
        raise RuntimeError("Run npm ci in canvas-app/chromium-shell before starting Phoenix.")
    preview = load("phoenix_monocode_assets", ROOT / "preview.py")
    frontend = load("phoenix_monocode_frontend", ROOT / "native-conversation-server.py")
    try:
        server = ThreadingHTTPServer(("127.0.0.1", 47845), frontend.handler(preview))
    except OSError as error:
        if error.errno != errno.EADDRINUSE:
            raise
        # Reuse Electron's single-instance handoff when the owned frontend is
        # already running, including when its window has been hidden to tray.
        with urlopen(frontend.ORIGIN + "/frontend-state", timeout=2) as response:
            active = json.load(response)
        if active.get("service") != "phoenix-native-conversation-v1":
            raise RuntimeError("Another application is using Phoenix's frontend port.") from error
        env = dict(os.environ, PHOENIX_CHROMIUM_CONVERSATION_URL=frontend.URL)
        electron = REPO / "canvas-app/chromium-shell/node_modules/electron/dist/electron"
        return subprocess.call([str(electron), str(REPO / "canvas-app/chromium-shell/main.cjs")], env=env)

    server.daemon_threads = True
    server.asset_root = ROOT / "native"
    server.presentation_handoff = frontend.load_handoff(os.environ.get("PHOENIX_UI_HANDOFF"))
    server.ui_connected = None
    server.ui_reported_at = 0
    threading.Thread(target=server.serve_forever, daemon=True).start()
    env = dict(os.environ)
    env["PHOENIX_GATEWAY_BINARY"] = str(gateway)
    env["PHOENIX_CHROMIUM_CONVERSATION_URL"] = frontend.URL
    env.pop("PHOENIX_CHROMIUM_SERVICES_ONLY", None)
    child = subprocess.Popen([str(desktop)], cwd=REPO, env=env)

    def stop(signum, _frame):
        if child.poll() is None:
            child.send_signal(signum)

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    print(f"Phoenix: {frontend.URL}", flush=True)
    try:
        return child.wait()
    finally:
        if child.poll() is None:
            child.terminate()
        server.shutdown()
        server.server_close()


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError) as error:
        raise SystemExit(str(error))
