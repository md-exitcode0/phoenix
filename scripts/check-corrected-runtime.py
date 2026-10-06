"""Dependency-free runtime package tests. No app, gateway or service launch."""
import gzip
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('corrected_runtime', ROOT / 'review-shell/launcher/corrected-runtime.py')
runtime = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runtime)


class RuntimePackageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='phoenix-runtime-test-')
        self.root = Path(self.temp.name)
        self.pack = self.root / 'pack'
        self.cache = self.root / 'cache'
        self.pack.mkdir()
        self.data = {'phoenix': b'fixture core', 'phoenix-desktop': b'fixture native'}
        self.manifest = {'version': 1, 'binaries': {}}
        for name, data in self.data.items():
            file = self.pack / (name + '.gz')
            file.write_bytes(gzip.compress(data))
            self.manifest['binaries'][name] = {'bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest(), 'compressed_sha256': runtime.digest(file)}
        self.save()

    def tearDown(self):
        self.temp.cleanup()

    def save(self):
        (self.pack / 'manifest.json').write_text(json.dumps(self.manifest))

    def test_exact_bytes_permissions_and_idempotent_cache(self):
        paths = runtime.materialize(self.pack, self.cache)
        for name, file in paths.items():
            self.assertEqual(Path(file).read_bytes(), self.data[name])
            self.assertEqual(Path(file).stat().st_mode & 0o777, 0o700)
        before = {name: Path(file).stat().st_ino for name, file in paths.items()}
        self.assertEqual(runtime.materialize(self.pack, self.cache), paths)
        self.assertEqual(before, {name: Path(file).stat().st_ino for name, file in paths.items()})

    def test_tampered_package_refused(self):
        (self.pack / 'phoenix.gz').write_bytes(b'tampered')
        with self.assertRaisesRegex(RuntimeError, 'compressed runtime changed'):
            runtime.materialize(self.pack, self.cache)

    def test_wrong_decompressed_identity_refused_without_partial_executable(self):
        self.manifest['binaries']['phoenix']['sha256'] = 'a' * 64
        self.save()
        with self.assertRaisesRegex(RuntimeError, 'integrity verification'):
            runtime.materialize(self.pack, self.cache)
        self.assertEqual(list(self.cache.iterdir()), [])

    def test_size_limit_prevents_compressed_expansion(self):
        self.manifest['binaries']['phoenix']['bytes'] = 1
        self.save()
        with self.assertRaisesRegex(RuntimeError, 'exceeded verified size'):
            runtime.materialize(self.pack, self.cache)
        self.assertEqual(list(self.cache.iterdir()), [])

    def test_manifest_cannot_select_paths(self):
        self.manifest['binaries']['../escaped'] = self.manifest['binaries'].pop('phoenix')
        self.save()
        with self.assertRaisesRegex(RuntimeError, 'Exact verified'):
            runtime.materialize(self.pack, self.cache)

    def test_symlink_package_and_cache_refused(self):
        link = self.root / 'link'
        link.symlink_to(self.pack)
        with self.assertRaisesRegex(RuntimeError, 'symbolic links'):
            runtime.materialize(link, self.cache)
        self.cache.symlink_to(self.pack)
        with self.assertRaisesRegex(RuntimeError, 'symbolic links'):
            runtime.materialize(self.pack, self.cache)

    def test_public_cache_refused(self):
        self.cache.mkdir(mode=0o755)
        with self.assertRaisesRegex(RuntimeError, 'owner-only'):
            runtime.materialize(self.pack, self.cache)

    def test_corrupted_cached_copy_replaced_without_deleting_other_files(self):
        paths = runtime.materialize(self.pack, self.cache)
        Path(paths['phoenix']).write_bytes(b'changed')
        other = self.cache / 'other-user-artifact'
        other.write_bytes(b'preserve')
        runtime.materialize(self.pack, self.cache)
        self.assertEqual(Path(paths['phoenix']).read_bytes(), self.data['phoenix'])
        self.assertEqual(other.read_bytes(), b'preserve')

    def test_exact_existing_service_target_does_not_restart(self):
        target = {'type': 'page', 'url': (self.root / 'phoenix_agent/canvas-app/ui/native-services.html').as_uri() + '?chromium=1'}
        with patch.object(runtime, 'targets', return_value=[target]), patch.object(runtime.subprocess, 'run') as run:
            self.assertEqual(runtime.ensure(self.root, self.pack), {'servicesOnly': True, 'started': False})
            run.assert_not_called()

    def test_exact_existing_native_frontend_does_not_restart(self):
        url='http://127.0.0.1:47845/?skin=monocode&chromium=1'
        with patch.object(runtime,'targets',return_value=[{'type':'page','url':url}]),patch.object(runtime.subprocess,'run') as run:
            self.assertEqual(runtime.ensure(self.root,self.pack,conversation_url=url),{'servicesOnly':False,'started':False})
            run.assert_not_called()

    def test_remote_or_unknown_frontend_cannot_select_native_host(self):
        with patch.object(runtime.subprocess,'run') as run:
            for url in ['https://example.com/','http://127.0.0.1:47845/?other=1']:
                with self.assertRaisesRegex(RuntimeError,'Exact owned'):
                    runtime.ensure(self.root,self.pack,conversation_url=url)
            run.assert_not_called()

    def test_disk_headroom_checked_before_decompression(self):
        class LowSpace:
            f_bavail = 1
            f_frsize = 1
        with patch.object(runtime.os, 'statvfs', return_value=LowSpace()):
            with self.assertRaisesRegex(RuntimeError, 'headroom'):
                runtime.materialize(self.pack, self.cache)
        self.assertEqual(list(self.cache.iterdir()), [])


if __name__ == '__main__':
    unittest.main()
