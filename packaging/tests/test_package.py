import importlib.util
from pathlib import Path
import struct
import subprocess
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location('package', Path(__file__).resolve().parents[1] / 'package.py')
package = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(package)


class PackagingTests(unittest.TestCase):
    def test_semver(self):
        self.assertEqual(package.versions('1.2.3'), '1.2.3')
        self.assertEqual(package.versions('1.2.3-rc.1'), '1.2.3~rc.1')
        for bad in ('v1.2.3', '1.2', '01.2.3', '1.2.3;touch /tmp/pwn', '1.2.3+build'):
            with self.assertRaises(ValueError):
                package.versions(bad)

    def elf(self, path, machine=62, kind=1, dependency=False):
        data = bytearray(152)
        data[:7] = b'\x7fELF\x02\x01\x01'
        struct.pack_into('<HH', data, 16, 2, machine)
        struct.pack_into('<Q', data, 32, 64)
        struct.pack_into('<HH', data, 54, 56, 1)
        struct.pack_into('<IIQQQQQQ', data, 64, kind, 0, 120, 0, 0, 32, 32, 8)
        if dependency:
            struct.pack_into('<qQ', data, 120, 1, 1)
        path.write_bytes(data)

    def test_static_elf_guards(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'binary'
            self.elf(path)
            package.check_elf(path, 'x86_64-unknown-linux-musl')
            with self.assertRaises(ValueError):
                package.check_elf(path, 'aarch64-unknown-linux-musl')
            for kind, dependency in ((3, False), (2, True)):
                self.elf(path, kind=kind, dependency=dependency)
                with self.assertRaises(ValueError):
                    package.check_elf(path, 'x86_64-unknown-linux-musl')
            path.write_bytes(b'not executable')
            with self.assertRaises(ValueError):
                package.check_elf(path, 'x86_64-unknown-linux-musl')

    def test_archive_determinism_and_symlink_rejection(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'pay-lmm'
            source.mkdir()
            (source / 'file').write_text('payload')
            package.archive(source, root / 'one.tar.gz', 123456)
            package.archive(source, root / 'two.tar.gz', 123456)
            self.assertEqual(package.sha256(root / 'one.tar.gz'), package.sha256(root / 'two.tar.gz'))
            (source / 'link').symlink_to('/etc/passwd')
            with self.assertRaises(ValueError):
                package.archive(source, root / 'bad.tar.gz', 123456)

    def test_shell_syntax(self):
        root = Path(__file__).resolve().parents[2]
        for path in (root / 'packaging').glob('*.sh'):
            subprocess.run(['sh' if path.name != 'build.sh' else 'bash', '-n', str(path)], check=True)
        subprocess.run(['sh', '-n', str(root / 'deploy/pay-lmm.openrc')], check=True)

    def test_installer_rejects_dangerous_prefix(self):
        installer = Path(__file__).resolve().parents[1] / 'install.sh'
        for prefix in ('/', '/tmp/../usr', 'relative'):
            result = subprocess.run(['sh', str(installer), '--prefix', prefix], capture_output=True)
            self.assertNotEqual(result.returncode, 0)


if __name__ == '__main__':
    unittest.main()
