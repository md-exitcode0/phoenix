#!/usr/bin/env python3
"""Serve the real MonoCode assets with an isolated renderer test hook."""
import importlib.util
from pathlib import Path
import tempfile
from http.server import ThreadingHTTPServer

ROOT = Path(__file__).resolve().parents[1] / "monocode"
spec = importlib.util.spec_from_file_location("monocode_fixture", ROOT / "preview.py")
preview = importlib.util.module_from_spec(spec)
spec.loader.exec_module(preview)


class Fixture(preview.Preview):
    def do_GET(self):
        if self.path.split("?")[0] == "/conversation.js":
            source = (ROOT / "ui/conversation.js").read_text()
            hook = "window.MonocodeRoomTest={ui,state,clearFeed,ensureWorkCluster,renderGroupMessage,renderIncomingAgentTalk,renderAnswer,renderInspectionBrowserTabs,detectMention,renderComposerText,syncGroupPals,layoutGroupPals,palStageFor,palState,setWorking,syncSendMode,submitTurn};"
            source = source.replace("  window.PhoenixConversation=", "  " + hook + "\n  window.PhoenixConversation=", 1)
            body = source.encode()
            self.send_preview_headers(200, "text/javascript", len(body))
            self.wfile.write(body)
            return
        super().do_GET()


with tempfile.TemporaryDirectory(prefix="phoenix-monocode-fixture-") as temporary:
    with ThreadingHTTPServer(("127.0.0.1", 0), Fixture) as server:
        server.asset_root = ROOT / "native"
        server.fixture_file = Path(temporary) / "fixture-state.json"
        print(server.server_port, flush=True)
        server.serve_forever()
