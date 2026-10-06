"""Isolated Fluffies consumer. Python stdlib, loopback only. No Phoenix gateway.
--asset-root may override the bundled pinned native assets.
Fixture writes stay beside this launcher. Native files are read-only and hash checked.
"""
from http.server import ThreadingHTTPServer, BaseHTTPRequestHandler
from pathlib import Path
from urllib.parse import urlsplit, unquote
import argparse, hashlib, json, mimetypes, os, threading
ROOT = Path(__file__).resolve().parent
PINS = json.loads((ROOT / 'native-pins.json').read_text())
FAMILY_PINS = json.loads((ROOT / 'family-pins.json').read_text())
LOCK = threading.Lock()
# Custom RGB was superseded. Only finished preset packs are served.
CSP = "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; connect-src 'self'; frame-src 'none'; object-src 'none'; base-uri 'self'; form-action 'self'"
def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()
def safe_target(base, raw):
    rel = unquote(raw).lstrip('/')
    if '\\' in rel or any(p in ('.', '..') for p in rel.split('/')):
        return None
    candidate = base / rel
    if not candidate.resolve().is_relative_to(base.resolve()):
        return None
    if any(p.is_symlink() for p in (candidate, *candidate.parents) if p != base.parent):
        return None
    return candidate if candidate.is_file() else None
class Preview(BaseHTTPRequestHandler):
    def send_preview_headers(self, status, content_type='application/json', size=None):
        self.send_response(status)
        self.send_header('Content-Type', content_type)
        self.send_header('Content-Security-Policy', CSP)
        self.send_header('X-Content-Type-Options', 'nosniff')
        self.send_header('Cache-Control', 'no-store')
        if size is not None: self.send_header('Content-Length', str(size))
        self.end_headers()
    def json(self, value, status=200):
        data=json.dumps(value).encode();self.send_preview_headers(status,size=len(data));self.wfile.write(data)
    def local(self):
        return self.headers_host() in (f'127.0.0.1:{self.server.server_port}', f'localhost:{self.server.server_port}')
    def headers_host(self):
        return self.headers.get('Host', '')
    def do_GET(self):
        if not self.local():self.json({'error':'Local host required'},403);return
        path=urlsplit(self.path).path
        if path == '/fixture-state':
            with LOCK:
                value=json.loads((self.server.fixture_file).read_text()) if (self.server.fixture_file).exists() else {'profiles':{},'images':{},'commands':[]}
            self.json(value);return
        if path == '/native-family/index.json':
            self.json(json.loads((ROOT/'family-pins.json').read_text()));return
        if path.startswith('/native-family/'):
            parts=unquote(path[len('/native-family/'):]).split('/',1)
            family=json.loads((ROOT/'family-pins.json').read_text()).get('families',{}).get(parts[0]) if len(parts)==2 else None
            if not family or 'butter' not in family.get('completedPalettes',[]) or family.get('renderVerification',{}).get('pass') is not True:
                self.json({'error':'Native family is not rendered and verified'},404);return
            relative=parts[1]
            expected=family.get('files',{}).get(relative)
            target=safe_target(ROOT/'native-family-v5'/parts[0],relative) if expected else None
            if target is None:self.json({'error':'Unpinned native family asset'},404);return
            data=target.read_bytes()
            if hashlib.sha256(data).hexdigest()!=expected:self.json({'error':'Native family pin mismatch'},409);return
            self.send_preview_headers(200,mimetypes.guess_type(target)[0] or 'application/octet-stream',len(data));self.wfile.write(data);return
        native=path.startswith('/native/')
        if native:
            relative=unquote(path[len('/native/'):])
            if relative not in PINS['files']: self.json({'error':'Unpinned asset'},404);return
            target=safe_target(self.server.asset_root,relative)
        else:
            target=safe_target(ROOT/'ui', '/index.html' if path == '/' else path)
        if target is None:self.json({'error':'Not found'},404);return
        data=target.read_bytes()
        if native and hashlib.sha256(data).hexdigest()!=PINS['files'][relative]:self.json({'error':'Native pin mismatch'},409);return
        self.send_preview_headers(200,mimetypes.guess_type(target)[0] or 'application/octet-stream',len(data));self.wfile.write(data)
    def do_POST(self):
        origin=f'http://{self.headers_host()}'
        if not self.local() or self.headers.get('Origin')!=origin:self.json({'error':'Same-origin fixture only'},403);return
        if urlsplit(self.path).path != '/fixture-directory':self.json({'error':'No production endpoint'},405);return
        try:length=int(self.headers.get('Content-Length','0'))
        except ValueError:self.json({'error':'Invalid fixture payload size'},400);return
        if length<=0 or length>17_000_000:self.json({'error':'Invalid fixture payload size'},413);return
        try:
            request=json.loads(self.rfile.read(length))
            if not isinstance(request,dict):raise ValueError('Fixture object required')
            with LOCK:
                path=self.server.fixture_file
                value=json.loads(path.read_text()) if path.exists() else {'profiles':{},'images':{},'commands':[]}
                if 'FixtureImage' in request:
                    image=request['FixtureImage'];data=image['dataUrl']
                    if not data.startswith(('data:image/png;base64,','data:image/jpeg;base64,','data:image/webp;base64,','data:image/avif;base64,')):raise ValueError('Unsupported fixture image')
                    image_id='avatar-fixture-'+hashlib.sha256(data.encode()).hexdigest()[:20]+'.png';value['images'][image_id]=data;answer={'imageId':image_id}
                else:
                    command=request['CompanyDirectory']
                    if not isinstance(command,dict) or command.get('action')!='update_agent':raise ValueError('Only Configure update_agent is persisted')
                    avatar=command.get('avatar') or {}
                    if not isinstance(avatar,dict):raise ValueError('Avatar config object required')
                    if avatar and avatar.get('mode') not in ('morph','custom','flame','sidekick'):raise ValueError('Unknown avatar mode')
                    if 'fluffy_palette' in avatar:raise ValueError('Frontend palette must not enter backend avatar payload')
                    agent=command['agent_id'];value['profiles'][agent]={**value['profiles'].get(agent,{}),**command};value['commands'].append(request);value['commands']=value['commands'][-64:];answer={'fixture':True,'saved':True}
                temp=path.with_suffix('.tmp');temp.write_text(json.dumps(value,indent=2)+'\n');os.replace(temp,path)
            self.json(answer)
        except (KeyError,ValueError,TypeError) as error:self.json({'error':str(error)},400)
    def log_message(self, format, *args):
        pass
if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('--port',type=int,default=0);parser.add_argument('--asset-root',type=Path,default=ROOT/'native');parser.add_argument('--fixture-name',default='fixture-state.json');options=parser.parse_args()
    if Path(options.fixture_name).name!=options.fixture_name or not options.fixture_name.endswith('.json'):parser.error('Fixture name must be a local JSON filename')
    assets=options.asset_root.resolve()
    for name in ('atlas.json','companion.js'):
        if not (assets/name).is_file() or digest(assets/name)!=PINS['files'][name]:parser.error('Use the frozen all-eight v19 web directory: '+name+' pin failed')
    with ThreadingHTTPServer(('127.0.0.1',options.port),Preview) as server:
        server.asset_root=assets
        server.fixture_file=ROOT/options.fixture_name
        print(f'Isolated Fluffies: http://127.0.0.1:{server.server_port}/?shot=calm-chat',flush=True)
        try:server.serve_forever()
        except KeyboardInterrupt:pass
