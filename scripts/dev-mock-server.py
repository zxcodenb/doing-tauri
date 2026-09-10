#!/usr/bin/env python3
"""Doing 客户端本地联调 mock：实现 /api/v1 快照契约（无 MySQL）。

模式（环境变量 DOING_MOCK_PUT）：
- conflict（默认）：PUT 恒 409 snapshot_conflict —— 演练「启动仲裁 → 冲突面板」；
- ok：PUT 成功并推进版本、云端内容随上传更新 —— 用于完整同步体验（登录 → 恢复 → 编辑 → 已同步）。

GET /api/v1/snapshot  → 200 云端快照（初始 1 条, version=9）
PUT /api/v1/snapshot  → 见上模式
POST auth/*           → 签发/轮换假令牌（任意用户名密码）
"""
import json
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PUT_MODE = os.environ.get("DOING_MOCK_PUT", "conflict")

STATE = {
    "items": [
        {
            "id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            "text": "云端的事项",
            "done": False,
            "createdAt": "2026-09-08T00:00:00Z",
            "dueDate": None,
            "updatedAt": "2026-09-08T00:00:00Z",
        }
    ],
    "focusId": None,
    "version": 9,
    "updatedAt": None,
}
TOKENS = json.dumps({"access": "mock-access", "refresh": "mock-refresh", "expiresIn": 900})


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def _send(self, code: int, body: str):
        data = body.encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def _read_body(self) -> bytes:
        length = int(self.headers.get("Content-Length") or 0)
        return self.rfile.read(length) if length > 0 else b""

    def log_message(self, fmt, *args):
        with open("/tmp/doing-mock.log", "a") as f:
            f.write("%s %s\n" % (self.command, self.path))

    def do_GET(self):
        if self.path == "/api/v1/snapshot":
            self._send(200, json.dumps(STATE, ensure_ascii=False))
        else:
            self._send(404, '{"code":"not_found","message":"no"}')

    def do_PUT(self):
        body = self._read_body()
        if PUT_MODE != "ok":
            self._send(
                409,
                json.dumps(
                    {
                        "code": "snapshot_conflict",
                        "message": "snapshot changed on another device",
                        "currentVersion": STATE["version"],
                    }
                ),
            )
            return
        try:
            payload = json.loads(body or b"{}")
        except json.JSONDecodeError:
            payload = {}
        if isinstance(payload.get("items"), list):
            STATE["items"] = payload["items"]
        STATE["focusId"] = payload.get("focusId")
        STATE["version"] = int(payload.get("baseVersion", STATE["version"])) + 1
        STATE["updatedAt"] = "2026-09-10T00:00:00Z"
        self._send(200, json.dumps({"version": STATE["version"], "updatedAt": STATE["updatedAt"]}))

    def do_POST(self):
        self._read_body()
        if self.path in ("/api/v1/auth/login", "/api/v1/auth/register", "/api/v1/auth/refresh"):
            self._send(200, TOKENS)
        elif self.path == "/api/v1/auth/logout":
            self._send(200, "{}")
        else:
            self._send(404, '{"code":"not_found","message":"no"}')


if __name__ == "__main__":
    print(f"mock 已启动: http://127.0.0.1:8080 （PUT 模式: {PUT_MODE}）", flush=True)
    ThreadingHTTPServer(("127.0.0.1", 8080), Handler).serve_forever()
