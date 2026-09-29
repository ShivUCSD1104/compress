"""Black-box safety tests; COMPRESS_REAL=1 also exercises installed compressors."""
import os
from pathlib import Path
import shutil
import signal
import struct
import subprocess
import tempfile
import time
import unittest
import zlib

ROOT = Path(__file__).resolve().parents[1]
BIN = ROOT / 'target/release/compress'
PNG = b'\x89PNG\r\n\x1a\n'


def png():
    def chunk(tag, data):
        return struct.pack('>I', len(data)) + tag + data + struct.pack('>I', zlib.crc32(tag + data))
    return PNG + chunk(b'IHDR', struct.pack('>IIBBBBB', 128, 128, 8, 2, 0, 0, 0)) + chunk(b'IDAT', zlib.compress((b'\0' + b'\xff\0\0' * 128) * 128, 0)) + chunk(b'IEND', b'')


def pdf():
    stream = b'0 0 0 rg\n' * 5000
    objects = [b'<< /Type /Catalog /Pages 2 0 R >>', b'<< /Type /Pages /Kids [3 0 R] /Count 1 >>', b'<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << >> /Contents 4 0 R >>', b'<< /Length ' + str(len(stream)).encode() + b' >>\nstream\n' + stream + b'endstream']
    data = b'%PDF-1.4\n'; offsets = [0]
    for i, obj in enumerate(objects, 1):
        offsets.append(len(data)); data += f'{i} 0 obj\n'.encode() + obj + b'\nendobj\n'
    start = len(data)
    data += b'xref\n0 5\n0000000000 65535 f \n'
    data += b''.join(f'{x:010} 00000 n \n'.encode() for x in offsets[1:])
    return data + f'trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{start}\n%%EOF\n'.encode()


class CLI(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.tools = self.root / 'bin'; self.tools.mkdir()
        self.env = dict(os.environ, PATH=str(self.tools))
        self.file = self.root / 'photo.png'; self.file.write_bytes(PNG + b'x' * 1000)

    def fake(self, tool='oxipng', body=None):
        import sys
        path = self.tools / tool
        path.write_text(f'#!{sys.executable}\nimport os, sys, time\nfrom pathlib import Path\np = Path(sys.argv[-1])\n' + (body or 'p.write_bytes(p.read_bytes()[:16])\n'))
        path.chmod(0o755)

    def call(self, *files):
        return subprocess.run([str(BIN), *map(str, files)], cwd=self.root, env=self.env, capture_output=True, timeout=10)

    def clean(self):
        self.assertEqual(list(self.root.glob('.compress-*')), [])

    def test_smaller_and_original_preserved(self):
        self.fake(); before = self.file.read_bytes()
        self.assertEqual(self.call(self.file).returncode, 0)
        self.assertEqual(self.file.read_bytes(), before)
        self.assertEqual((self.root / 'photo.compressed.png').stat().st_size, 16)
        self.clean()

    def test_no_savings(self):
        self.fake(body='pass\n')
        self.assertIn(b'unchanged', self.call(self.file).stdout)
        self.assertFalse((self.root / 'photo.compressed.png').exists()); self.clean()

    def test_existing_output_and_dangling_symlink(self):
        self.fake(); out = self.root / 'photo.compressed.png'; out.write_bytes(b'keep')
        self.assertEqual(self.call(self.file).returncode, 1); self.assertEqual(out.read_bytes(), b'keep')
        out.unlink(); out.symlink_to(self.root / 'missing')
        self.assertEqual(self.call(self.file).returncode, 1); self.assertTrue(out.is_symlink())

    def test_failure_and_wrong_format(self):
        for body in ['p.write_bytes(b"bad"); sys.exit(1)\n', 'p.write_bytes(b"bad")\n']:
            self.fake(body=body); self.assertEqual(self.call(self.file).returncode, 1)
            self.assertFalse((self.root / 'photo.compressed.png').exists()); self.clean()

    def test_missing_backend(self):
        r = self.call(self.file); self.assertEqual(r.returncode, 1)
        self.assertIn(b'brew install oxipng', r.stderr); self.clean()

    def test_content_routing_and_awkward_names(self):
        self.fake()
        for name in ['-photo.JPG', 'a space;$(touch HACK).pdf', 'unicode-\u2603']:
            p = self.root / name; p.write_bytes(self.file.read_bytes())
            self.assertEqual(self.call('--', name).returncode, 0)
        self.assertFalse((self.root / 'HACK').exists())

    def test_invalid_inputs_and_batch_continuation(self):
        self.fake(); bad = self.root / 'bad.pdf'; bad.write_text('hello')
        link = self.root / 'link.png'; link.symlink_to(self.file)
        r = self.call(bad, link, self.root, 'missing', self.file)
        self.assertEqual(r.returncode, 1); self.assertTrue((self.root / 'photo.compressed.png').exists())

    def test_concurrent_output_creation(self):
        out = self.root / 'photo.compressed.png'
        self.fake(body=f'Path({str(out)!r}).write_bytes(b"competitor")\np.write_bytes(p.read_bytes()[:16])\n')
        self.assertEqual(self.call(self.file).returncode, 1)
        self.assertEqual(out.read_bytes(), b'competitor'); self.clean()

    def test_interrupt(self):
        ready = self.root / 'ready'
        self.fake(body=f'Path({str(ready)!r}).touch()\ntime.sleep(30)\n')
        p = subprocess.Popen([str(BIN), str(self.file)], env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            deadline = time.monotonic() + 5
            while not ready.exists() and time.monotonic() < deadline: time.sleep(.01)
            self.assertTrue(ready.exists()); p.send_signal(signal.SIGINT); p.communicate(timeout=5)
            self.assertEqual(p.returncode, 130); self.clean()
            self.assertFalse((self.root / 'photo.compressed.png').exists())
        finally:
            if p.poll() is None: p.kill(); p.wait()

    def test_quality_arguments(self):
        for options in [('--quality',), ('--quality', 'bad'), ('--quality=',), ('--lossless', '--quality', 'small'), ('--quality', 'high', '--lossless')]:
            self.assertEqual(self.call(*options).returncode, 2)
        self.fake()
        self.assertEqual(self.call('--quality=small', self.file).returncode, 0)

    def test_jpeg_quality_and_lossless_routing(self):
        p = self.root / 'sample.jpg'
        p.write_bytes(b'\xff\xd8\xff' + b'x' * 1000)
        log = self.root / 'calls'
        self.fake('jpegoptim', body=f'with open({str(log)!r}, "a") as f: f.write(" ".join(sys.argv[1:]) + "\\n")\np.write_bytes(p.read_bytes()[:16])\n')
        for options, expected in [((), '--max=80'), (('--quality', 'high'), '--max=90'), (('--quality', 'small'), '--max=60'), (('--lossless',), None)]:
            log.write_text('')
            self.assertEqual(self.call(*options, p).returncode, 0)
            calls = log.read_text()
            if expected: self.assertIn(expected, calls)
            else: self.assertNotIn('--max=', calls)
            (self.root / 'sample.compressed.jpg').unlink()

    def test_usage(self):
        self.assertEqual(self.call().returncode, 2)
        self.assertEqual(self.call('--wat').returncode, 2)
        self.assertEqual(self.call('--help').returncode, 0)
        self.assertIn(b'0.2.0', self.call('--version').stdout)


@unittest.skipUnless(os.getenv('COMPRESS_REAL') == '1', 'set COMPRESS_REAL=1 for real backend tests')
class Real(unittest.TestCase):
    def test_formats(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for ext, data in [('png', png()), ('pdf', pdf())]:
                p = root / ('sample.' + ext); p.write_bytes(data)
                r = subprocess.run([str(BIN), '--lossless', str(p)], capture_output=True)
                self.assertEqual(r.returncode, 0, r.stderr)
                out = root / ('sample.compressed.' + ext)
                self.assertLess(out.stat().st_size, len(data)); self.assertEqual(p.read_bytes(), data)
            if shutil.which('sips'):
                jpg = root / 'sample.jpg'
                subprocess.run(['sips', '-s', 'format', 'jpeg', str(root / 'sample.png'), '--out', str(jpg)], check=True, capture_output=True)
                before = jpg.read_bytes()
                r = subprocess.run([str(BIN), str(jpg)], capture_output=True)
                self.assertEqual(r.returncode, 0, r.stderr); self.assertEqual(jpg.read_bytes(), before)
            for ext, data in [('pdf', b'%PDF-1.4\nbroken'), ('png', PNG + b'broken'), ('jpg', b'\xff\xd8\xffbroken')]:
                p = root / ('broken.' + ext); p.write_bytes(data)
                r = subprocess.run([str(BIN), '--lossless', str(p)], capture_output=True)
                self.assertEqual(r.returncode, 1, r.stdout)
                self.assertFalse((root / ('broken.compressed.' + ext)).exists())

if __name__ == '__main__': unittest.main(verbosity=2)
