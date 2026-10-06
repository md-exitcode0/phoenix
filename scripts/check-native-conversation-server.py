"""No app, profile, credentials, native IPC or network used."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
import io
from types import SimpleNamespace

ROOT=Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location('native_frontend',ROOT/'review-shell/launcher/native-conversation-server.py')
frontend=importlib.util.module_from_spec(spec)
spec.loader.exec_module(frontend)


class NativeFrontendTests(unittest.TestCase):
    def state_request(self, value, origin=frontend.ORIGIN, path='/frontend-state'):
        class Preview:
            def local(self):return True
            def json(self,value,status=200):self.reply=(status,value)
        cls=frontend.handler(SimpleNamespace(Preview=Preview))
        req=cls();payload=json.dumps(value).encode();req.headers={'Origin':origin,'Content-Length':str(len(payload))};req.path=path;req.rfile=io.BytesIO(payload);req.server=SimpleNamespace(ui_connected=None)
        req.do_POST();return req

    def test_readiness_requires_explicit_success(self):
        for value in (True,False):
            req=self.state_request({'connected':value})
            self.assertEqual(req.reply,(200,{'ok':True}));self.assertIs(req.server.ui_connected,value)

    def test_readiness_cannot_accept_account_data_or_cross_origin(self):
        for value,origin in [({'connected':True,'token':'fixture'},frontend.ORIGIN),({'connected':'true'},frontend.ORIGIN),({'connected':True},'https://example.org')]:
            req=self.state_request(value,origin)
            self.assertIn(req.reply[0],(400,405));self.assertIsNone(req.server.ui_connected)

    def test_presentation_and_scoped_draft_retained_exactly(self):
        value={'phoenix-avatar-presentation-v1':json.dumps({'version':1,'agents':{}}),'phoenix-composer-draft:agent:phoenix':'fixture draft','phoenix-theme':'light'}
        self.assertEqual(frontend.presentation(value),value)

    def test_credentials_or_arbitrary_keys_cannot_move(self):
        for key in ['token','phoenix-auth','phoenix-provider-credentials','../escape','phoenix-composer-draft:../escape']:
            with self.assertRaises(ValueError):frontend.presentation({key:'fixture'})

    def test_non_string_and_oversized_payload_refused(self):
        for value in [[],{'phoenix-theme':{}},{'phoenix-theme':'x'*(1024*1024+1)}]:
            with self.assertRaises(ValueError):frontend.presentation(value)

    def test_handoff_requires_owner_only_regular_path(self):
        with tempfile.TemporaryDirectory() as folder:
            path=Path(folder)/'presentation.json';path.write_text('{"phoenix-theme":"dark"}');path.chmod(0o600)
            self.assertEqual(frontend.load_handoff(str(path)),{'phoenix-theme':'dark'})
            path.chmod(0o644)
            with self.assertRaises(ValueError):frontend.load_handoff(str(path))
            path.chmod(0o600);link=Path(folder)/'link';link.symlink_to(path)
            with self.assertRaises(ValueError):frontend.load_handoff(str(link))

    def test_native_csp_allows_local_gateway_and_site_icons_only(self):
        self.assertIn("connect-src 'self' ws://127.0.0.1:*;",frontend.CSP)
        self.assertNotIn('ws://*',frontend.CSP)
        self.assertNotIn('wss:',frontend.CSP)
        self.assertIn("img-src 'self' http: https: data: blob:;",frontend.CSP)
        self.assertIn("frame-src 'none'",frontend.CSP)
        self.assertNotIn('unsafe-eval',frontend.CSP)


if __name__=='__main__':unittest.main()
