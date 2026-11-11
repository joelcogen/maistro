"""Installer smoke tests; never build real code or change the user's services."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class InstallTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.checkout = root / 'source checkout'
        self.home = root / 'home with spaces'
        self.tools = root / 'tools'
        for path in (self.checkout, self.home, self.tools):
            path.mkdir()
        repo = Path(__file__).resolve().parents[1]
        for name in ('install.sh', 'config.example.toml', 'systemd/maistro.service', 'examples/project-prompt.md'):
            target = self.checkout / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(repo / name, target)
        cargo = self.tools / 'cargo'
        cargo.write_text('''#!/bin/bash
set -eu
test "$*" = 'build --release --locked'
[ "${FAIL_BUILD:-0}" = 0 ] || exit 21
mkdir -p target/release
printf '#!/bin/sh\\nexit 0\\n' > target/release/maistro
''')
        cargo.chmod(0o755)
        systemctl = self.tools / 'systemctl'
        systemctl.write_text('#!/bin/sh\necho unexpected-service-operation >&2\nexit 99\n')
        systemctl.chmod(0o755)
        self.env = dict(os.environ, HOME=str(self.home), PATH=f'{self.tools}:/usr/bin:/bin')
        self.config = self.home / '.config/maistro'

    def install(self, **env):
        return subprocess.run([str(self.checkout / 'install.sh')], cwd=self.home,
                              env=dict(self.env, **env), input='', text=True, capture_output=True)

    def test_install_without_credentials_or_starting_service(self):
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(os.access(self.home / '.local/bin/maistro', os.X_OK))
        self.assertTrue((self.home / '.config/systemd/user/maistro.service').exists())
        self.assertTrue((self.config / 'prompts/example.md').exists())
        self.assertFalse((self.config / 'environment').exists())
        self.assertEqual((self.config / 'config.toml').stat().st_mode & 0o777, 0o600)
        self.assertIn('systemctl --user start maistro', result.stdout)

    def test_preserves_existing_configuration_and_environment(self):
        self.config.mkdir(parents=True)
        (self.config / 'config.toml').write_text('old config needing migration\n')
        (self.config / 'environment').write_text('PATH=/custom/bin\n')
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.config / 'config.toml').read_text(), 'old config needing migration\n')
        self.assertEqual((self.config / 'environment').read_text(), 'PATH=/custom/bin\n')
        self.assertIn('require migration', result.stdout)

    def test_build_failure_stops_install(self):
        result = self.install(FAIL_BUILD='1')
        self.assertEqual(result.returncode, 21)
        self.assertFalse((self.home / '.local/bin/maistro').exists())


if __name__ == '__main__':
    unittest.main()
