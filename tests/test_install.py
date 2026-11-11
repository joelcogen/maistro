"""Installer integration tests using local tools; no network or real credentials.

Run with: python3 -m unittest discover -s tests -p 'test_install.py'
"""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class InstallTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        root = Path(self.temporary.name)
        self.checkout = root / "source checkout"
        self.checkout.mkdir()
        self.home = root / "home with spaces"
        self.home.mkdir()
        self.config = self.home / ".config/maistro"
        self.tools = root / "tools"
        self.tools.mkdir()
        repository = Path(__file__).resolve().parents[1]
        shutil.copy2(repository / "install.sh", self.checkout)
        shutil.copy2(repository / "config.example.toml", self.checkout)
        self.tool("cargo", """
test "$*" = 'build --release --locked' || exit 90
printf 'build\n' >> "$HOME/events"
[ "${FAIL_BUILD:-0}" = 0 ] || exit 21
mkdir -p target/release
cp "$FAKE_BINARY" target/release/maistro
""")
        self.tool("fake-maistro", """
test "$#" = 3 && test "$1" = -c && test "$3" = validate || exit 91
test "$2" = "$HOME/.config/maistro/config.toml" || exit 92
test "$MAISTRO_GITHUB_TOKEN" = ghp_test_123 || exit 93
grep -q '# edited' "$2" || exit 94
printf 'validate\n' >> "$HOME/events"
exit "${FAIL_VALIDATE:-0}"
""")
        self.tool("editor", """
test "$1" = --wait && test "$#" = 2 || exit 95
printf 'edit\n' >> "$HOME/events"
[ "${FAIL_EDITOR:-0}" = 0 ] || exit 22
printf '\n# edited\n' >> "$2"
""")
        self.env = dict(os.environ, HOME=str(self.home),
                        PATH=f"{self.tools}:/usr/bin:/bin", EDITOR="editor --wait",
                        FAKE_BINARY=str(self.tools / "fake-maistro"))

    def tool(self, name, body):
        path = self.tools / name
        path.write_text("#!/bin/bash\nset -eu\n" + body)
        path.chmod(0o755)

    def run_install(self, token="ghp_test_123\n", **variables):
        result = subprocess.run(
            [str(self.checkout / "install.sh")], cwd=self.home,
            env=dict(self.env, **variables), input=token, text=True,
            capture_output=True,
        )
        self.assertNotIn("ghp_test_123", result.stdout + result.stderr)
        return result

    def test_install_edits_then_validates_with_private_credentials(self):
        result = self.run_install()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("https://github.com/settings/tokens/new", result.stdout)
        self.assertEqual((self.home / "events").read_text(), "build\nedit\nvalidate\n")
        self.assertEqual(self.config.stat().st_mode & 0o777, 0o700)
        for name in ("config.toml", "environment"):
            self.assertEqual((self.config / name).stat().st_mode & 0o777, 0o600)
        self.assertEqual((self.config / "environment").read_text().strip(),
                         "MAISTRO_GITHUB_TOKEN=ghp_test_123")
        self.assertTrue(os.access(self.home / ".local/bin/maistro", os.X_OK))

    def test_rerun_preserves_configuration_and_other_environment_settings(self):
        self.config.mkdir(parents=True)
        (self.config / "config.toml").write_text("# existing config\n")
        (self.config / "environment").write_text(
            "OTHER=value\nMAISTRO_GITHUB_TOKEN=old\nexport MAISTRO_GITHUB_TOKEN=older")
        self.assertEqual(self.run_install().returncode, 0)
        self.assertTrue((self.config / "config.toml").read_text().startswith("# existing config"))
        environment = (self.config / "environment").read_text()
        self.assertIn("OTHER=value\n", environment)
        self.assertNotIn("old", environment)
        self.assertEqual(environment.count("MAISTRO_GITHUB_TOKEN="), 1)

    def test_local_config_is_used_on_first_install(self):
        (self.checkout / "config.toml").write_text("# pilot config\n")
        self.assertEqual(self.run_install().returncode, 0)
        self.assertTrue((self.config / "config.toml").read_text().startswith("# pilot config"))

    def test_failures_propagate_and_stop_later_steps(self):
        for variable, code, events in (
            ("FAIL_BUILD", 21, "build\n"),
            ("FAIL_EDITOR", 22, "build\nedit\n"),
            ("FAIL_VALIDATE", 23, "build\nedit\nvalidate\n"),
        ):
            with self.subTest(variable=variable):
                (self.home / "events").write_text("")
                result = self.run_install(**{variable: str(code)})
                self.assertEqual(result.returncode, code)
                self.assertEqual((self.home / "events").read_text(), events)
                self.assertNotIn("Installation and validation succeeded", result.stdout)

    def test_empty_or_shell_input_is_rejected_and_eof_stops(self):
        result = self.run_install("\n$(touch should-not-exist)\n")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.checkout / "should-not-exist").exists())
        self.assertFalse((self.config / "environment").exists())
        self.assertEqual((self.home / "events").read_text(), "build\n")

    def test_unset_editor_falls_back_to_vi(self):
        self.tool("vi", 'printf "\\n# edited\\n" >> "$1"\n')
        self.env.pop("EDITOR")
        self.assertEqual(self.run_install().returncode, 0)


if __name__ == "__main__":
    unittest.main()
