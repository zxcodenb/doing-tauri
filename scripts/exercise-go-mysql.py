#!/usr/bin/env python3
"""Exercise the Rust coordinator against unmodified Go routes and a NEW MySQL.

No DATABASE_URL/default MySQL configuration, system service, real app data,
Keychain, Docker daemon or existing database is used. See tests/go-mysql/README.md.
"""
from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import secrets
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from urllib.parse import urlparse

REPO = Path(__file__).resolve().parent.parent
KIND = "doing-tauri-isolated-go-mysql-v1"
TEST = "go_mysql_tests::isolated_go_mysql_contract"


class FixtureError(RuntimeError):
    pass


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, new_url):
        return None


def source_files(source: Path) -> list[Path]:
    """Copy only the imported Go module/runtime/migrations, never config/secrets."""
    files = [source / "go.mod", source / "go.sum"]
    files += sorted(
        p for p in (source / "internal").rglob("*.go")
        if "config" not in p.relative_to(source).parts and not p.name.endswith("_test.go")
    )
    files += sorted((source / "migrations").glob("*.sql"))
    required = source / "internal/server/router.go"
    if required not in files or not any(p.suffix == ".sql" for p in files):
        raise FixtureError("server source must contain the original router and migrations")
    for path in files:
        relative = path.relative_to(source)
        parents = [source.joinpath(*relative.parts[:i]) for i in range(1, len(relative.parts) + 1)]
        if not path.is_file() or any(p.is_symlink() for p in parents):
            raise FixtureError("server source must contain regular, non-symlink files")
    return files


def source_hash(source: Path) -> str:
    digest = hashlib.sha256()
    for path in source_files(source):
        digest.update(path.relative_to(source).as_posix().encode() + b"\0")
        digest.update(hashlib.sha256(path.read_bytes()).digest())
    return digest.hexdigest()


def child_environment() -> dict[str, str]:
    excluded = {"DATABASE_URL", "JWT_SECRET", "GOWORK", "GOFLAGS", "DYLD_LIBRARY_PATH"}
    proxy = {"http_proxy", "https_proxy", "all_proxy", "no_proxy"}
    env = {
        k: v for k, v in os.environ.items()
        if k not in excluded and k.lower() not in proxy
        and not k.startswith(("DOING_", "MYSQL"))
    }
    env.update(GOWORK="off", GOFLAGS="-mod=readonly", NO_PROXY="127.0.0.1,localhost")
    return env


def private_json(path: Path, data: object) -> None:
    with tempfile.NamedTemporaryFile(mode="w", dir=path.parent, delete=False) as f:
        staged = Path(f.name)
        try:
            json.dump(data, f, ensure_ascii=False, indent=2)
            f.write("\n")
            f.flush()
            os.fsync(f.fileno())
            os.replace(staged, path)
        finally:
            staged.unlink(missing_ok=True)


class Child:
    def __init__(self, label: str, args: list[str], cwd: Path, env: dict[str, str], log: Path):
        self.label = label
        self.log = log
        with log.open("ab") as output:
            self.process = subprocess.Popen(
                args, cwd=cwd, env=env, stdout=output, stderr=subprocess.STDOUT,
                stdin=subprocess.DEVNULL, start_new_session=True,
            )
        self.pid = self.process.pid

    def wait(self, timeout: int = 1200) -> None:
        try:
            code = self.process.wait(timeout=timeout)
        except subprocess.TimeoutExpired as exc:
            raise FixtureError(f"{self.label} timed out (log: {self.log.name})") from exc
        if code != 0:
            raise FixtureError(f"{self.label} exited {code} (log: {self.log.name})")

    def alive(self) -> None:
        if self.process.poll() is not None:
            raise FixtureError(f"{self.label} stopped unexpectedly (log: {self.log.name})")

    def stop(self) -> None:
        if self.process.poll() is not None:
            return
        # Only the child we started, never a process-name/pidfile lookup or pkill.
        self.process.terminate()
        try:
            self.process.wait(timeout=30)
        except subprocess.TimeoutExpired as exc:
            self.process.kill()
            self.process.wait(timeout=10)
            raise FixtureError(f"{self.label} required forced termination") from exc
        if self.process.returncode != 0:
            raise FixtureError(f"{self.label} did not stop cleanly ({self.process.returncode})")


def wait_for(predicate, child: Child, stage: str, timeout: int = 60):
    until = time.monotonic() + timeout
    while time.monotonic() < until:
        child.alive()
        value = predicate()
        if value:
            return value
        time.sleep(0.1)
    raise FixtureError(f"{stage} timed out")


def readiness(root: Path, nonce: str):
    path = root / "ready.json"
    if not path.exists():
        return None
    data = json.loads(path.read_text())
    url = urlparse(data["baseUrl"])
    if (data.get("kind") != KIND or data.get("nonce") != nonce
            or url.scheme != "http" or url.hostname != "127.0.0.1" or not url.port
            or url.username or url.password or url.path or url.query or url.fragment):
        raise FixtureError("invalid fixture readiness; refusing to contact an unknown API")
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    try:
        with opener.open(data["baseUrl"] + "/api/v1/health/ready", timeout=2) as response:
            if response.status == 200 and response.headers.get("X-Doing-Test-Fixture") == nonce:
                return data
    except (OSError, urllib.error.URLError):
        pass
    return None


def version(args: list[str], env: dict[str, str]) -> str:
    return subprocess.check_output(args, env=env, text=True, stderr=subprocess.STDOUT, timeout=30).strip()


def exercise(args, root: Path, report: dict) -> None:
    env = child_environment()
    mysql_env = dict(env)
    if args.mysql_library_path:
        mysql_env["DYLD_LIBRARY_PATH"] = args.mysql_library_path
        mysql_env["LD_LIBRARY_PATH"] = args.mysql_library_path
    base = args.mysql_basedir.resolve(strict=True)
    for name in ("mysqld",):
        binary = base / "bin" / name
        if not binary.is_file() or not os.access(binary, os.X_OK):
            raise FixtureError(f"--mysql-basedir needs executable bin/{name}")
    source = args.server_source.resolve(strict=True)
    before = source_hash(source)
    report.update(
        serverSourceSha256=before,
        versions={
            "mysql": version([str(base / "bin/mysqld"), "--no-defaults", "--version"], mysql_env)
                .replace(str(base / "bin/mysqld"), "mysqld", 1),
            "go": version(["go", "version"], env),
            "rust": version(["rustc", "--version"], env),
        },
    )
    if shutil.disk_usage(root).free < 2 * 1024**3:
        raise FixtureError("at least 2 GiB free space is required for a fresh MySQL fixture")
    (root / "fixture-kind").write_text(KIND)
    (root / "data").mkdir(mode=0o700)
    copied = root / "server"
    copied.mkdir(mode=0o700)
    for path in source_files(source):
        target = copied / path.relative_to(source)
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, target)
        target.chmod(0o444)
    if source_hash(copied) != before:
        raise FixtureError("server source changed while creating the read-only fixture copy")
    harness = copied / "cmd/doing-tauri-contract/main.go"
    harness.parent.mkdir(parents=True)
    shutil.copyfile(REPO / "tests/go-mysql/main.go", harness)
    children: list[Child] = []

    def start(label, command, *, cwd=root, child_env=env):
        child = Child(label, command, cwd, child_env, root / f"{label}.log")
        children.append(child)
        return child

    cleanup_errors = []
    try:
        print("[1/6] Build temporary launcher with original Go runtime sources", flush=True)
        start("go-build", ["go", "build", "-o", str(root / "go-api"), "./cmd/doing-tauri-contract"], cwd=copied).wait()
        print("[2/6] Initialize NEW socket-only MySQL (no system service)", flush=True)
        start("mysql-initialize", [
            str(base / "bin/mysqld"), "--no-defaults", f"--basedir={base}",
            f"--datadir={root / 'data'}", "--initialize-insecure", "--mysqlx=0",
        ], child_env=mysql_env).wait(timeout=180)
        nonce = secrets.token_hex(32)
        api_env = dict(env, DOING_TEST_FIXTURE_ROOT=str(root), DOING_TEST_FIXTURE_NONCE=nonce,
                       DOING_TEST_JWT_SECRET=secrets.token_hex(32), DOING_TEST_API_PORT="0")
        user_suffix = secrets.token_hex(6)
        manifest = {
            "kind": KIND, "nonce": nonce, "accessTtlSeconds": 3,
            "username": "Contract_" + user_suffix, "otherUsername": "Other_" + user_suffix,
            "password": secrets.token_urlsafe(24),
        }
        report["phases"] = []
        for index, phase in enumerate(("workflows", "process-restart")):
            print(f"[{3 + index * 2}/6] Start MySQL + Go on loopback; run Rust {phase}", flush=True)
            mysql = start(f"mysql-{index}", [
                str(base / "bin/mysqld"), "--no-defaults", f"--basedir={base}",
                f"--datadir={root / 'data'}", f"--socket={root / 'mysql.sock'}",
                f"--pid-file={root / 'mysql.pid'}", f"--log-error={root / f'mysql-error-{index}.log'}",
                "--skip-networking", "--mysqlx=0", "--skip-log-bin", "--performance-schema=OFF",
                "--innodb-buffer-pool-size=32M", "--max-connections=20",
            ], child_env=mysql_env)

            def mysql_ready():
                log = root / f"mysql-error-{index}.log"
                # Per-process log, not stale readiness from the previous run.
                # The Go launcher then verifies @@datadir/@@skip_networking via SQL.
                return ((root / "mysql.sock").exists() and log.is_file()
                        and "ready for connections" in log.read_text(errors="replace"))

            wait_for(mysql_ready, mysql, "MySQL readiness")
            (root / "ready.json").unlink(missing_ok=True)
            (root / "audit.json").unlink(missing_ok=True)
            api = start(f"go-api-{index}", [str(root / "go-api")], cwd=copied, child_env=api_env)
            ready = wait_for(lambda: readiness(root, nonce), api, "Go/MySQL readiness")
            if ready["pid"] != api.pid:
                raise FixtureError("fixture identity does not match the process we launched")
            manifest["baseUrl"] = ready["baseUrl"]
            # Reuse the same origin after restart: a different port is a different owner.
            api_env["DOING_TEST_API_PORT"] = str(urlparse(ready["baseUrl"]).port)
            private_json(root / "contract-manifest.json", manifest)
            test_env = dict(env, DOING_GO_MYSQL_TEST_MANIFEST=str(root / "contract-manifest.json"),
                            DOING_GO_MYSQL_TEST_PHASE=phase)
            rust = start(f"rust-{phase}", [
                "cargo", "test", "-p", "doing-desktop", "--lib", "--locked", TEST, "--",
                "--ignored", "--exact", "--nocapture", "--test-threads=1",
            ], cwd=REPO, child_env=test_env)
            rust.wait()
            # Verify an exact test actually ran; cargo's zero-matching-tests exit 0 is not evidence.
            output = rust.log.read_text(errors="replace")
            if "test result: ok. 1 passed; 0 failed;" not in output:
                raise FixtureError("the protected Rust integration entry did not run exactly once")
            print(f"[{4 + index * 2}/6] Stop only owned processes and capture aggregate SQL audit", flush=True)
            api.stop()
            mysql.stop()
            audit = json.loads((root / "audit.json").read_text())
            if audit["migrations"] != [1, 2, 3]:
                raise FixtureError("original Go migrations were not applied exactly once")
            phase_report = {
                "phase": phase, "mysqlPid": mysql.pid, "goPid": api.pid, "rustPid": rust.pid,
                "passed": True, "cleanShutdown": True, "audit": audit,
            }
            if phase == "workflows":
                isolated = json.loads((root / "evidence-uuid-isolation.json").read_text())
                if (isolated["users"], isolated["items"], isolated["sharedItemIds"]) != (2, 6, 3):
                    raise FixtureError("missing real SQL evidence for same UUIDs across accounts")
                phase_report["uuidIsolationCheckpoint"] = isolated
            report["phases"].append(phase_report)
        report["serverSourceUnchanged"] = source_hash(source) == before
        if not report["serverSourceUnchanged"]:
            raise FixtureError("original Go source changed during the run")
        report["status"] = "passed"
    finally:
        for child in reversed(children):
            try:
                child.stop()
            except FixtureError as exc:
                cleanup_errors.append(str(exc))
        report["allOwnedProcessesStopped"] = all(c.process.poll() is not None for c in children)
        if cleanup_errors:
            report["cleanupErrors"] = cleanup_errors
            raise FixtureError("one or more fixture processes did not stop cleanly")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mysql-basedir", type=Path, required=True,
                        help="installed/extracted MySQL 8.4 base (bin/mysqld)")
    parser.add_argument("--mysql-library-path", default="",
                        help="explicit library search path for an unrelocated bottle, MySQL children only")
    parser.add_argument("--server-source", type=Path, default=REPO.parent / "doing/server")
    parser.add_argument("--report", type=Path, help="write sanitized JSON evidence here")
    parser.add_argument("--keep-artifacts", action="store_true", help="keep private SYNTHETIC fixture files for debugging")
    args = parser.parse_args()
    if os.name != "posix":
        parser.error("this runner requires Unix sockets; Windows native integration is a separate gate")
    # Use a short private path: Darwin Unix socket paths have a small fixed limit.
    root = Path(tempfile.mkdtemp(prefix="doing-contract-", dir="/tmp")).resolve()
    root.chmod(0o700)
    report = {
        "kind": KIND, "startedAt": dt.datetime.now(dt.timezone.utc).isoformat(), "status": "failed",
        "evidenceScope": "Rust app state/coordinator + original Go router + isolated MySQL; not native OS/installation acceptance",
    }
    def interrupted(*_):
        raise KeyboardInterrupt

    old_term = signal.signal(signal.SIGTERM, interrupted)
    result = 1
    try:
        exercise(args, root, report)
        result = 0
    except (FixtureError, OSError, subprocess.SubprocessError, ValueError) as exc:
        print(f"Fixture failed: {exc}", file=sys.stderr)
    except KeyboardInterrupt:
        print("Fixture interrupted; stopping owned processes", file=sys.stderr)
    finally:
        signal.signal(signal.SIGTERM, old_term)
        report["finishedAt"] = dt.datetime.now(dt.timezone.utc).isoformat()
        if args.keep_artifacts:
            print(f"Private synthetic artifacts: {root}")
        elif not report.get("allOwnedProcessesStopped", True):
            print(f"Cleanup incomplete; private fixture retained at {root}", file=sys.stderr)
            result = 1
        else:
            shutil.rmtree(root)
            report["temporaryFilesRemoved"] = True
        if result != 0:
            report["status"] = "failed"
        if args.report:
            args.report.parent.mkdir(parents=True, exist_ok=True)
            private_json(args.report, report)
    if result == 0:
        print("PASS: real Go/MySQL workflows + MySQL/Go/Rust process restart; no system service or real credentials used")
    return result


if __name__ == "__main__":
    raise SystemExit(main())
