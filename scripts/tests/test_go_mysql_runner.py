"""Offline guard tests. No MySQL/API/network is started by this suite."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock

SCRIPT = Path(__file__).resolve().parents[1] / "exercise-go-mysql.py"
SPEC = importlib.util.spec_from_file_location("go_mysql_runner", SCRIPT)
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)


class SafetyGuards(unittest.TestCase):
    def source(self, root):
        for name, content in {
            "go.mod": "module doing-server\n",
            "go.sum": "",
            "internal/server/router.go": "package server\n",
            "internal/config/config.go": "synthetic excluded configuration",
            "internal/server/router_test.go": "package server\n",
            "migrations/0001_init.sql": "CREATE TABLE fixture (id INT);",
        }.items():
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content)

    def test_copy_does_not_include_config_or_go_tests(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.source(root)
            selected = {p.relative_to(root).as_posix() for p in runner.source_files(root)}
            self.assertEqual(selected, {"go.mod", "go.sum", "internal/server/router.go", "migrations/0001_init.sql"})
            before = runner.source_hash(root)
            (root / "internal/config/config.go").write_text("changed excluded configuration")
            self.assertEqual(runner.source_hash(root), before)
            (root / "internal/server/router.go").write_text("changed original route")
            self.assertNotEqual(runner.source_hash(root), before)

    def test_copy_rejects_symlinked_source(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.source(root)
            path = root / "internal/server/router.go"
            path.unlink()
            path.symlink_to(root / "go.mod")
            with self.assertRaises(runner.FixtureError):
                runner.source_files(root)

    def test_missing_original_router_is_not_a_valid_fixture(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.source(root)
            (root / "internal/server/router.go").unlink()
            with self.assertRaises(runner.FixtureError):
                runner.source_files(root)

    def test_env_cannot_inherit_real_database_credentials_or_proxy(self):
        inherited = {
            "DATABASE_URL": "not-a-real-secret", "JWT_SECRET": "not-a-real-secret",
            "DOING_API_URL": "https://not-a-fixture.invalid", "DOING_SKIP_LOGIN": "1",
            "MYSQL_PWD": "not-a-real-secret", "HTTP_PROXY": "https://not-a-fixture.invalid",
            "https_proxy": "https://not-a-fixture.invalid", "ALL_PROXY": "https://not-a-fixture.invalid",
            "DYLD_LIBRARY_PATH": "/unrelated", "PATH": "/tools", "GOFLAGS": "-mod=mod",
        }
        with mock.patch.dict(os.environ, inherited, clear=True):
            env = runner.child_environment()
        for key in inherited:
            if key not in {"PATH", "GOFLAGS"}:
                self.assertNotIn(key, env)
        self.assertEqual(env["PATH"], "/tools")
        self.assertEqual(env["GOWORK"], "off")
        self.assertEqual(env["GOFLAGS"], "-mod=readonly")
        self.assertEqual(env["NO_PROXY"], "127.0.0.1,localhost")

    def test_readiness_rejects_wrong_kind_nonce_and_origin_before_any_network(self):
        valid = {"kind": runner.KIND, "nonce": "fixture-nonce", "baseUrl": "http://127.0.0.1:33333"}
        invalid = [
            {"kind": "unknown"}, {"nonce": "different"},
            *({"baseUrl": url} for url in (
                "https://example.invalid", "http://localhost:33333", "http://0.0.0.0:33333",
                "http://127.0.0.1", "http://127.0.0.1:0", "http://user:pass@127.0.0.1:33333",
                "http://127.0.0.1:33333/api", "http://127.0.0.1:33333?key=x", "http://127.0.0.1:33333#x",
            )),
        ]
        with tempfile.TemporaryDirectory() as temp, mock.patch.object(runner.urllib.request, "build_opener") as network:
            root = Path(temp)
            for changes in invalid:
                (root / "ready.json").write_text(json.dumps(dict(valid, **changes)))
                with self.assertRaises(runner.FixtureError):
                    runner.readiness(root, "fixture-nonce")
            network.assert_not_called()

    def test_readiness_never_follows_redirects(self):
        self.assertIsNone(runner.NoRedirect().redirect_request(
            None, None, 302, "redirect", {}, "https://not-a-fixture.invalid"
        ))

    def test_reports_and_manifests_are_private_atomic_json(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "report.json"
            runner.private_json(path, {"status": "first"})
            runner.private_json(path, {"status": "second"})
            self.assertEqual(json.loads(path.read_text()), {"status": "second"})
            self.assertEqual(path.stat().st_mode & 0o077, 0)
            self.assertEqual(list(path.parent.iterdir()), [path])


if __name__ == "__main__":
    unittest.main()
