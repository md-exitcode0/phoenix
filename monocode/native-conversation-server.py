"""Serve the revised conversation inside Phoenix's existing native window.

The native preload, gateway, browser surfaces and profiles remain authoritative.
Only pinned frontend assets and a bounded presentation handoff are served here.
"""
import json
import os
from pathlib import Path
import re
import time

ORIGIN = 'http://127.0.0.1:47845'
URL = ORIGIN + '/?skin=monocode&chromium=1'
# The native gateway_status command supplies the user's configured port.
# Keep sockets on IPv4 loopback without assuming the default gateway port.
CSP = "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' http: https: data: blob:; font-src 'self' data:; connect-src 'self' ws://127.0.0.1:*; frame-src 'none'; object-src 'none'; base-uri 'self'; form-action 'self'"
KEYS = {'phoenix-theme', 'phoenix-visual', 'phoenix-avatar-presentation-v1', 'phoenix-review-avatar-seeded', 'phoenix-sidebar-collapsed', 'phoenix-sidebar-width'}
SCOPED = re.compile(r'^phoenix-(?:composer-draft|reading-position|inspection|pending-submission|permission|model|model-context|reasoning):[a-zA-Z0-9_:.\-]{1,512}$')


def presentation(value):
    if not isinstance(value, dict) or len(value) > 512:
        raise ValueError('Bounded presentation handoff required')
    if any(not isinstance(key, str) or (key not in KEYS and not SCOPED.fullmatch(key)) or not isinstance(item, str) for key, item in value.items()):
        raise ValueError('Only known presentation and draft keys may move')
    if len(json.dumps(value).encode()) > 1024 * 1024:
        raise ValueError('Presentation handoff exceeds its limit')
    return value


def load_handoff(raw):
    if not raw:
        return {}
    path = Path(raw).absolute()
    if any(p.is_symlink() for p in (path, *path.parents)) or path.stat().st_uid != os.getuid() or path.stat().st_mode & 0o077 or path.stat().st_size > 1024 * 1024:
        raise ValueError('Owner-only presentation handoff required')
    return presentation(json.loads(path.read_text()))


def handler(preview):
    class NativeConversation(preview.Preview):
        def send_preview_headers(self, status, content_type='application/json', size=None):
            self.send_response(status)
            self.send_header('Content-Type', content_type)
            self.send_header('Content-Security-Policy', CSP)
            self.send_header('X-Content-Type-Options', 'nosniff')
            self.send_header('Cache-Control', 'no-store')
            if size is not None:
                self.send_header('Content-Length', str(size))
            self.end_headers()

        def do_GET(self):
            if not self.local():
                self.json({'error': 'Local host required'}, 403)
                return
            if self.path.split('?')[0] == '/frontend-state':
                self.json({'service':'phoenix-native-conversation-v1','connected':self.server.ui_connected is True and time.monotonic()-self.server.ui_reported_at<15})
                return
            if self.path.split('?')[0] == '/fixture-state':
                self.json({'error': 'Native conversation uses the existing company'}, 404)
                return
            if self.path.split('?')[0] == '/backend-bootstrap.js':
                values = json.dumps(presentation(self.server.presentation_handoff))
                bootstrap = "(()=>{if(!localStorage.getItem('phoenix-native-conversation-adopted-v1')){const values=" + values + ";for(const [key,value] of Object.entries(values))localStorage.setItem(key,value);localStorage.setItem('phoenix-native-conversation-adopted-v1','1');}document.documentElement.dataset.backend='native';let connected=false,ready=false;const report=()=>{if(ready)fetch('/frontend-state',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({connected})}).catch(()=>{});};addEventListener('phoenix:directory-ready',e=>{connected=!e.detail.runtimeError;ready=true;report();});setInterval(report,3000);})();\n"
                body = (bootstrap + (preview.ROOT / 'ui/backend-bootstrap.js').read_text()).encode()
                self.send_preview_headers(200, 'text/javascript', len(body))
                self.wfile.write(body)
                return
            super().do_GET()

        def do_POST(self):
            if self.local() and self.path == '/frontend-state' and self.headers.get('Origin') == ORIGIN:
                try:
                    size = int(self.headers.get('Content-Length', '0'))
                    if not 0 < size <= 64:
                        raise ValueError('Bounded frontend state required')
                    value = json.loads(self.rfile.read(size))
                    if set(value) != {'connected'} or not isinstance(value['connected'], bool):
                        raise ValueError('Boolean frontend state required')
                    self.server.ui_connected = value['connected']
                    self.server.ui_reported_at=time.monotonic()
                except (ValueError, TypeError):
                    self.json({'error': 'Invalid frontend state'}, 400)
                    return
                self.json({'ok': True})
                return
            self.json({'error': 'Use Phoenix native commands'}, 405)

    return NativeConversation
