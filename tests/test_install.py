"""Installer smoke tests; never build real code or change the user's services."""
import os
from pathlib import Path
import pty
import select
import shutil
import subprocess
import tempfile
import time
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
        self.token = 'github_pat_install_fixture'
        self.env = dict(os.environ, HOME=str(self.home), PATH=f'{self.tools}:/usr/bin:/bin',
                        MAISTRO_GITHUB_TOKEN=self.token)
        self.config = self.home / '.config/maistro'

    def install(self, **env):
        return subprocess.run([str(self.checkout / 'install.sh')], cwd=self.home,
                              env=dict(self.env, **env), input='', text=True, capture_output=True)

    def test_install_saves_private_token_without_starting_service(self):
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(os.access(self.home / '.local/bin/maistro', os.X_OK))
        self.assertTrue((self.home / '.config/systemd/user/maistro.service').exists())
        self.assertTrue((self.config / 'prompts/example.md').exists())
        self.assertFalse((self.config / 'environment').exists())
        self.assertEqual((self.config / 'github-token').read_text(), self.token + '\n')
        self.assertEqual((self.config / 'github-token').stat().st_mode & 0o777, 0o600)
        self.assertEqual(self.config.stat().st_mode & 0o777, 0o700)
        self.assertNotIn(self.token, result.stdout + result.stderr)
        for permission in ('Contents', 'Workflows', 'Pull requests'):
            self.assertIn(permission + ': Read and write', result.stdout)
        self.assertEqual((self.config / 'config.toml').stat().st_mode & 0o777, 0o600)
        self.assertIn('systemctl --user start maistro', result.stdout)

    def test_preserves_existing_configuration_and_environment(self):
        self.config.mkdir(parents=True)
        (self.config / 'config.toml').write_text('old config needing migration\n')
        (self.config / 'environment').write_text('PATH=/custom/bin\n')
        (self.config / 'github-token').write_text('github_pat_existing\n')
        result = self.install(MAISTRO_GITHUB_TOKEN='')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.config / 'config.toml').read_text(), 'old config needing migration\n')
        self.assertEqual((self.config / 'environment').read_text(), 'PATH=/custom/bin\n')
        self.assertEqual((self.config / 'github-token').read_text(), 'github_pat_existing\n')
        self.assertIn('require migration', result.stdout)

    def test_build_failure_stops_install(self):
        result = self.install(FAIL_BUILD='1')
        self.assertEqual(result.returncode, 21)
        self.assertFalse((self.home / '.local/bin/maistro').exists())

    def test_noninteractive_install_requires_token(self):
        result = self.install(MAISTRO_GITHUB_TOKEN='')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Token required', result.stderr)
        self.assertFalse((self.config / 'github-token').exists())

    def test_invalid_token_does_not_replace_existing_secret_or_execute_input(self):
        self.assertEqual(self.install().returncode, 0)
        for invalid in ('two lines\nsecret', 'spaces secret', '$(touch injected)', 'a' * 4097):
            result = self.install(MAISTRO_GITHUB_TOKEN=invalid)
            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn(invalid, result.stdout + result.stderr)
            self.assertEqual((self.config / 'github-token').read_text(), self.token + '\n')
        self.assertFalse((self.checkout / 'injected').exists())

    def test_explicit_replacement_rotates_token_and_does_not_leak_under_shell_trace(self):
        self.assertEqual(self.install().returncode, 0)
        replacement = 'github_pat_replacement'
        result = subprocess.run(['bash', '-x', str(self.checkout / 'install.sh')],
                                env=dict(self.env, MAISTRO_GITHUB_TOKEN=replacement),
                                input='', text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.config / 'github-token').read_text(), replacement + '\n')
        self.assertNotIn(replacement, result.stdout + result.stderr)
        self.assertEqual(list(self.config.glob('.github-token.*')), [])

    def test_refuses_symlink_without_overwriting_target(self):
        self.config.mkdir(parents=True)
        target = self.home / 'existing-secret'
        target.write_text('preserve me')
        (self.config / 'github-token').symlink_to(target)
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(target.read_text(), 'preserve me')

    def test_interactive_input_is_hidden_and_saved(self):
        master, slave = pty.openpty()
        self.addCleanup(os.close, master)
        self.addCleanup(os.close, slave)
        process = subprocess.Popen([str(self.checkout / 'install.sh')], stdin=slave,
                                   stdout=slave, stderr=slave,
                                   env=dict(self.env, MAISTRO_GITHUB_TOKEN=''))
        self.addCleanup(lambda: process.kill() if process.poll() is None else None)
        output = b''
        deadline = time.monotonic() + 10
        while b'Paste the GitHub token (hidden):' not in output:
            self.assertLess(time.monotonic(), deadline, output)
            if select.select([master], [], [], 0.1)[0]:
                output += os.read(master, 4096)
        os.write(master, (self.token + '\n').encode())
        self.assertEqual(process.wait(timeout=10), 0)
        while select.select([master], [], [], 0.1)[0]:
            output += os.read(master, 4096)
        self.assertNotIn(self.token.encode(), output)
        self.assertEqual((self.config / 'github-token').read_text(), self.token + '\n')


if __name__ == '__main__':
    unittest.main()
