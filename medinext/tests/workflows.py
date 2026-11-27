#!/usr/bin/env python3
"""End-to-end integration tests for MediNext v2.0.0 (Mediary 3.4.6 native playback_hook + media-service + range-sha1)."""

import base64
import http.server
import json
import os
import shutil
import socket
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time


def run_action(
    binary: str,
    data_dir: str,
    action: str,
    payload: dict,
    api_url: str = "",
    token: str = "mp1_test_token",
    settings: dict | None = None,
) -> dict:
    env = {
        "PATH": "/usr/local/bin:/usr/bin:/bin",
        "MEDIARY_PLUGIN_ID": "medinext",
        "MEDIARY_PLUGIN_DATA_DIR": data_dir,
        "MEDIARY_PLUGIN_ACTION": action,
        "MEDIARY_PLUGIN_TRIGGER": "manual",
        "MEDIARY_PLUGIN_PLAYBACK_HOOK_APPROVED": "true",
        "MEDIARY_PLUGIN_SETTINGS_JSON": json.dumps(
            settings
            if settings is not None
            else {
                # Mediary Web UI strips "$enabled" from settings (storing "enabled" at top-level of config.json)
                "auto_clean_quota": True,
                "notify": False,
            }
        ),
    }
    if api_url:
        env["MEDIARY_PLUGIN_API_URL"] = api_url
        env["MEDIARY_PLUGIN_TOKEN"] = token
    proc = subprocess.run(
        [binary],
        input=json.dumps(payload).encode("utf-8"),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
        timeout=20,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"Action {action} failed ({proc.returncode}): {proc.stderr.decode()}")
    return json.loads(proc.stdout.decode("utf-8").strip())


def build_sample_nextemby_db(path: str):
    conn = sqlite3.connect(path)
    conn.executescript(
        """
        CREATE TABLE settings (key TEXT, value TEXT, category TEXT);
        CREATE TABLE accounts (username TEXT, cookies TEXT, proxy TEXT, cache_path TEXT, auto_delete_cron TEXT, recycle_bin_code TEXT, is_master INTEGER, drive_info TEXT, sort_order INTEGER);
        CREATE TABLE lb_accounts (username TEXT, cookies TEXT, proxy TEXT, cache_path TEXT, auto_delete_cron TEXT, recycle_bin_code TEXT, is_master INTEGER, transfer_quota INTEGER, transfer_quota_enabled INTEGER, transfer_quota_usage INTEGER, is_valid INTEGER, drive_info TEXT, is_enabled INTEGER, sort_order INTEGER);
        CREATE TABLE users (username TEXT, password TEXT, emby_id TEXT, cookies TEXT, p115_uid TEXT, proxy TEXT, expire_at REAL, expiry_date TEXT, add_date TEXT, cache_path TEXT, is_admin INTEGER, is_temporary INTEGER, created_at REAL, template TEXT, auto_delete_cron TEXT, recycle_bin_code TEXT, transfer_quota INTEGER, transfer_quota_enabled INTEGER, transfer_quota_usage INTEGER, remark TEXT, reg_ip_location TEXT, is_disabled INTEGER, is_captain INTEGER, drive_info TEXT, captain_invite_code TEXT, is_banned INTEGER, expired_check_count INTEGER);
        """
    )
    conn.execute(
        "INSERT INTO settings VALUES ('template_policies', '[{\"name\":\"vvvvvip\",\"max_streams\":2,\"max_devices\":2,\"default_route\":\"负载均衡\"}]', 'emby')"
    )
    conn.execute(
        "INSERT INTO lb_accounts VALUES ('115-lb1', 'UID=80000002_F3_1; CID=ccc; SEID=ddd', '', '/Next', '', '', 0, 5, 1, 10737418240, 1, '', 1, 0)"
    )
    conn.execute(
        "INSERT INTO lb_accounts VALUES ('115-lb1-ipad', 'UID=80000002_B1_1; CID=eee; SEID=fff', '', '/Next', '', '', 0, 5, 1, 0, 1, '', 1, 1)"
    )
    # Note: legacy_emby_uuid simulates old Emby UUIDs before Mediary numeric ID alignment
    conn.execute(
        "INSERT INTO users (username, emby_id, cookies, p115_uid, expiry_date, cache_path, is_admin, template, is_disabled, is_banned) VALUES ('alice', 'legacy_uuid_alice', '\"负载均衡\"', 'load_balance', '2099-12-31', '/最近接收', 0, 'vvvvvip', 0, 0)"
    )
    conn.execute(
        "INSERT INTO users (username, emby_id, cookies, p115_uid, expiry_date, cache_path, is_admin, template, is_disabled, is_banned) VALUES ('expired_bob', 'legacy_uuid_bob', '\"负载均衡\"', 'load_balance', '2020-01-01', '/最近接收', 0, 'vvvvvip', 0, 0)"
    )
    conn.commit()
    conn.close()


def find_free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def unix_http_request(sock_path: str, method: str, path: str, payload: dict | None = None) -> tuple[int, dict]:
    body = json.dumps(payload).encode("utf-8") if payload is not None else b""
    req = (
        f"{method} {path} HTTP/1.1\r\n"
        f"Host: localhost\r\n"
        f"Content-Type: application/json\r\n"
        f"Content-Length: {len(body)}\r\n"
        f"Connection: close\r\n\r\n"
    ).encode("utf-8") + body

    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as cli:
        cli.settimeout(5)
        cli.connect(sock_path)
        cli.sendall(req)
        chunks = []
        while True:
            data = cli.recv(4096)
            if not data:
                break
            chunks.append(data)
    raw = b"".join(chunks)
    head, resp_body = raw.split(b"\r\n\r\n", 1)
    status_code = int(head.splitlines()[0].split()[1])
    return status_code, json.loads(resp_body.decode("utf-8"))


def main():
    binary = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else "./plugin")
    assert os.path.isfile(binary) and os.access(binary, os.X_OK), f"Binary not executable: {binary}"

    tmp_dir = tempfile.mkdtemp(prefix="medinext_v2_test_")
    kicked_keys: list[str] = []
    disabled_users: list[tuple[str, bool]] = []
    sent_notifications: list[dict] = []
    active_playbacks: list[dict] = [
        {
            "session_key": "sess_alice_tv1",
            "user_id": 10,
            "username": "alice",
            "item_id": "item_demo_1",
            "item_name": "Demo Movie 4K",
            "client": "Infuse",
            "device_id": "tv-1",
            "device_name": "Apple TV Living",
            "user_agent": "Infuse-Direct/8.0",
            "client_ip": "10.0.0.21",
            "started_at": "2026-11-27 06:00:00",
            "last_seen_at": "2026-11-27 06:05:00",
        },
        {
            "session_key": "sess_alice_tv2",
            "user_id": 10,
            "username": "alice",
            "item_id": "item_demo_2",
            "item_name": "Demo Episode",
            "client": "SenPlayer",
            "device_id": "tv-2",
            "device_name": "iPad Pro",
            "user_agent": "SenPlayer/5.0",
            "client_ip": "10.0.0.22",
            "started_at": "2026-11-27 06:01:00",
            "last_seen_at": "2026-11-27 06:05:00",
        },
    ]

    class MockMediary346(http.server.BaseHTTPRequestHandler):
        def _send_json(self, code: int, obj: dict):
            raw = json.dumps(obj).encode("utf-8")
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)

        def do_GET(self):
            if self.path == "/api/media-service/users":
                return self._send_json(
                    200,
                    {
                        "users": [
                            {"id": 1, "username": "admin", "is_admin": True, "enabled": True},
                            {"id": 10, "username": "alice", "is_admin": False, "enabled": True},
                            {"id": 11, "username": "expired_bob", "is_admin": False, "enabled": True},
                            {"id": 12, "username": "charlie", "is_admin": False, "enabled": True},
                        ]
                    },
                )
            if self.path == "/api/media-service/active-playbacks":
                return self._send_json(200, {"playbacks": active_playbacks})
            if self.path.startswith("/api/plugin/library/items/"):
                item_part = self.path.split("/api/plugin/library/items/", 1)[1].split("?")[0]
                if item_part == "series_demo_tv":
                    return self._send_json(
                        200,
                        {
                            "item": {
                                "id": "series_demo_tv",
                                "name": "Demo TV Series",
                                "item_type": "series",
                                "tmdb_id": 999888,
                            }
                        },
                    )
                if item_part == "ep_demo_s01e01":
                    return self._send_json(
                        200,
                        {
                            "item": {
                                "id": "ep_demo_s01e01",
                                "name": "Demo.TV.S01E01.mkv",
                                "item_type": "episode",
                                "series_id": "series_demo_tv",
                                "series_tmdb_id": 999888,
                                "season": 1,
                                "episode": 1,
                                "poster_path": "/demo_poster.jpg",
                                "sources": [
                                    {
                                        "id": "ms_ep1",
                                        "pick_code": "pcepisode0000001",
                                        "sha1": "1111111111111111111111111111111111111111",
                                        "size": 900 * 1024 * 1024,
                                    }
                                ],
                            }
                        },
                    )
                if item_part == "ep_demo_s01e02":
                    return self._send_json(
                        200,
                        {
                            "item": {
                                "id": "ep_demo_s01e02",
                                "name": "Demo.TV.S01E02.mkv",
                                "item_type": "episode",
                                "series_id": "series_demo_tv",
                                "series_tmdb_id": 999888,
                                "season": 1,
                                "episode": 2,
                                "sources": [
                                    {
                                        "id": "ms_ep2",
                                        "pick_code": "pcepisode0000002",
                                        "sha1": "2222222222222222222222222222222222222222",
                                        "size": 920 * 1024 * 1024,
                                    }
                                ],
                            }
                        },
                    )
                return self._send_json(
                    200,
                    {
                        "item": {
                            "id": "item_small_demo",
                            "name": "Small Clip.mp4",
                            "item_type": "movie",
                            "poster_path": "/small_poster.jpg",
                            "sources": [
                                {
                                    "id": "ms_1",
                                    "pick_code": "ay4u93dndfz6ehj39",
                                    "sha1": "DA39A3EE5E6B4B0D3255BFEF95601890AFD80709",
                                    "size": 50 * 1024 * 1024,
                                }
                            ],
                        }
                    },
                )
            if self.path.startswith("/api/plugin/library/items?"):
                return self._send_json(
                    200,
                    {
                        "items": [
                            {"id": "ep_demo_s01e02", "item_type": "episode", "season": 1, "episode": 2, "series_tmdb_id": 999888, "name": "E02"},
                            {"id": "ep_demo_s01e01", "item_type": "episode", "season": 1, "episode": 1, "series_tmdb_id": 999888, "name": "E01"},
                        ]
                    },
                )
            if self.path in ("/api/open115/status", "/api/open115/web-cookie/status"):
                return self._send_json(200, {"connected": True})
            if self.path == "/api/settings":
                return self._send_json(200, {"tmdb_proxy_url": ""})
            return self._send_json(404, {"error": "not found"})

        def do_POST(self):
            length = int(self.headers.get("Content-Length", "0"))
            body = json.loads(self.rfile.read(length) or b"{}")
            if self.path == "/api/plugin/open115/range-sha1":
                assert body["pick_code"] == "ay4u93dndfz6ehj39"
                return self._send_json(200, {"sha1": "ABCDEF0123456789ABCDEF0123456789ABCDEF01"})
            if self.path == "/api/plugin/notifications":
                sent_notifications.append(body)
                return self._send_json(200, {"ok": True})
            return self._send_json(200, {"ok": True})

        def do_PUT(self):
            length = int(self.headers.get("Content-Length", "0"))
            body = json.loads(self.rfile.read(length) or b"{}")
            if self.path.startswith("/api/media-service/users/") and self.path.endswith("/enabled"):
                uid = self.path.split("/")[4]
                disabled_users.append((uid, bool(body.get("enabled"))))
                return self._send_json(200, {"ok": True})
            return self._send_json(404, {})

        def do_DELETE(self):
            if self.path.startswith("/api/media-service/sessions/"):
                skey = self.path.split("/api/media-service/sessions/", 1)[1]
                kicked_keys.append(skey)
                active_playbacks[:] = [p for p in active_playbacks if p["session_key"] != skey]
                return self._send_json(200, {"ok": True})
            return self._send_json(404, {})

        def log_message(self, *args):
            pass

    mock_port = find_free_port()
    httpd = http.server.HTTPServer(("127.0.0.1", mock_port), MockMediary346)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    api_url = f"http://127.0.0.1:{mock_port}/api"

    try:
        data_dir = os.path.join(tmp_dir, "data")
        # Write plugin config.json beside data/ to verify scope/hook inspection
        with open(os.path.join(tmp_dir, "config.json"), "w", encoding="utf-8") as f:
            json.dump(
                {
                    "enabled": True,
                    "granted_scopes": ["media-library:read", "media-service:read", "media-service:write", "open115:read"],
                    "playback_hook_approved": True,
                    "pending_scope_approval": False,
                },
                f,
            )

        # 1. Bootstrap & dashboard.json (verify enabled detection when "$enabled" is stripped from settings!)
        boot = run_action(binary, data_dir, "ui_bootstrap", {}, api_url=api_url)
        assert boot["version"] == "2.1.1", boot["version"]
        assert boot["engine"]["notify_playback"] is False, boot["engine"]
        assert boot["host_status"]["plugin_enabled"] is True, boot["host_status"]
        assert boot["settings"]["$enabled"] is True, boot["settings"]
        assert boot["host_status"]["playback_hook_approved"] is True
        assert boot["host_status"]["media_service_read_ready"] is True
        assert len(boot["active_sessions"]) == 2, boot["active_sessions"]
        dash_path = os.path.join(data_dir, "dashboard.json")
        assert os.path.isfile(dash_path)
        dash_json = json.load(open(dash_path, "r", encoding="utf-8"))
        assert dash_json["summary"]["mode_summary"] == "原生 302 钩子 · 已生效", dash_json

        # 2. Import NextEmby SQLite config.db (without master source account -> optional!)
        sample_db = os.path.join(tmp_dir, "config.db")
        build_sample_nextemby_db(sample_db)
        b64 = base64.b64encode(open(sample_db, "rb").read()).decode("ascii")
        imp = run_action(
            binary,
            data_dir,
            "ui_import_nextemby",
            {
                "filename": "config.db",
                "file_base64": f"data:application/octet-stream;base64,{b64}",
            },
            api_url=api_url,
        )
        assert imp["summary"]["imported_lbs"] == 2
        assert imp["summary"]["imported_users"] == 2
        assert imp["bootstrap"]["lb_accounts"][0]["quota_gb"] == 5120
        # Verify ui_bootstrap reconciles same-UID multi-client LB quota_usage_bytes (10 GB synced to 115-lb1-ipad!)
        boot_after_imp = run_action(binary, data_dir, "ui_bootstrap", {}, api_url=api_url)
        assert boot_after_imp["lb_accounts"][1]["quota_usage_bytes"] == 10737418240, boot_after_imp["lb_accounts"]

        # 3. Sync users natively via Mediary media-service:read
        sync_res = run_action(binary, data_dir, "ui_sync_users", {}, api_url=api_url)
        assert len(sync_res["bootstrap"]["users"]) == 4

        # 4. Configure expired fallback_video_url & test Unix Domain Socket playback_hook daemon!
        run_action(
            binary,
            data_dir,
            "ui_save_engine",
            {
                "engine": {
                    "cache_ttl_secs": 1800,
                    "small_file_direct_mb": 200,
                    "p115_cooldown_ms": 250,
                    "expired_video_url": "https://cdn.example.com/expired.mp4",
                    "fallback_on_fail": True,
                    "fallback_for_guest": True,
                    "enable_next_ep_prewarm": True,
                    "enable_detail_prefetch": True,
                    "detail_prefetch_delay_ms": 10,
                    "notify_playback": True,
                    "notify_risk_intercept": True,
                }
            },
            api_url=api_url,
        )

        # Verify manual test notification action (`ui_test_notify`)
        nt_res = run_action(binary, data_dir, "ui_test_notify", {}, api_url=api_url)
        assert nt_res["ok"] is True
        assert any("已连通" in n.get("title", "") for n in sent_notifications), sent_notifications

        sock_path = os.path.join(data_dir, "playback-hook.sock")
        daemon_env = {
            "PATH": "/usr/local/bin:/usr/bin:/bin",
            "MEDIARY_PLUGIN_ID": "medinext",
            "MEDIARY_PLUGIN_DATA_DIR": data_dir,
            "MEDIARY_PLUGIN_API_URL": api_url,
            "MEDIARY_PLUGIN_TOKEN": "mp1_test_token",
            "MEDIARY_PLUGIN_PLAYBACK_SOCKET": sock_path,
            "MEDIARY_PLUGIN_PLAYBACK_HOOK_APPROVED": "true",
            "MEDIARY_PLUGIN_SETTINGS_JSON": json.dumps({"auto_clean_quota": False, "notify": True}),
        }
        daemon = subprocess.Popen([binary, "daemon"], env=daemon_env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            for _ in range(30):
                if os.path.exists(sock_path):
                    code, h = unix_http_request(sock_path, "GET", "/health")
                    if code == 200 and h.get("ok"):
                        break
                time.sleep(0.05)
            else:
                raise RuntimeError("Unix socket playback_hook did not start in time")

            # 4a-0. Host internal screenshot/probe task (`Mediary/3.4.6 media-screenshot`, empty user_id) -> immediate `pass`, NEVER recorded as `guest`!
            code, internal_pass = unix_http_request(
                sock_path,
                "POST",
                "/resolve",
                {
                    "pick_code": "pcepisode0000001",
                    "user_agent": "Mediary/3.4.6 media-screenshot",
                    "client_ip": "172.17.0.1",
                },
            )
            assert code == 200 and internal_pass["status"] == "pass", internal_pass

            # 4a. UA Blacklist (with duplicate size+file_size & null fields exactly as sent by Mediary 3.4.6!) -> status == "deny"
            code, ua_deny = unix_http_request(
                sock_path,
                "POST",
                "/resolve",
                {
                    "user_id": "10",
                    "username": None,
                    "is_admin": None,
                    "sha1": None,
                    "size": 82268396,
                    "file_size": 82268396,
                    "pickcode": "ay4u93dndfz6ehj39",
                    "pick_code": "ay4u93dndfz6ehj39",
                    "name": "Demo.mp4",
                    "file_name": "Demo.mp4",
                    "user_agent": "python-requests/2.31.0",
                    "device_id": "tv-1",
                    "client_ip": "10.0.0.21",
                },
            )
            assert code == 200 and ua_deny["status"] == "deny" and ua_deny["status_code"] == 403, ua_deny
            assert "fallback_video_url" not in ua_deny, ua_deny

            # 4b. Expired user (passed ONLY numeric Mediary user_id=11, no username!) -> status == "deny" + fallback_video_url!
            code, exp_deny = unix_http_request(
                sock_path,
                "POST",
                "/resolve",
                {
                    "user_id": 11,
                    "pick_code": "ay4u93dndfz6ehj39",
                    "user_agent": "Infuse-Direct/8.0",
                    "device_id": "dev-bob",
                },
            )
            assert code == 200 and exp_deny["status"] == "deny", exp_deny
            assert exp_deny.get("fallback_video_url") == "https://cdn.example.com/expired.mp4", exp_deny

            # 4c. Concurrency limit (alice has tv-1 & tv-2 active in Mediary active-playbacks, max_streams=2 -> tv-3 denied 429!)
            code, conc_deny = unix_http_request(
                sock_path,
                "POST",
                "/resolve",
                {
                    "user_id": 10,
                    "pick_code": "ay4u93dndfz6ehj39",
                    "user_agent": "Infuse-Direct/8.0",
                    "device_id": "tv-2",  # existing device is allowed!
                    "item_id": "item_small_demo",
                },
            )
            # Small file (50 MB <= 200 MB) without local source_accounts -> delegates cleanly (`status: "pass"`) to Mediary native /api/302!
            assert code == 200 and conc_deny["status"] == "pass", conc_deny

            # Register tv-1 & tv-2 devices first, then test max_devices=2 blocking tv-3
            unix_http_request(
                sock_path,
                "POST",
                "/resolve",
                {"user_id": 10, "pick_code": "ay4u93dndfz6ehj39", "user_agent": "Infuse/8.0", "device_id": "tv-1", "item_id": "item_small_demo"},
            )
            code, dev_deny = unix_http_request(
                sock_path,
                "POST",
                "/resolve",
                {"user_id": 10, "pick_code": "ay4u93dndfz6ehj39", "user_agent": "Infuse/8.0", "device_id": "tv-3"},
            )
            assert code == 200 and dev_deny["status"] == "deny" and dev_deny["status_code"] == 403, dev_deny

            # 4d. True Session Kick via `DELETE /api/media-service/sessions/:key`
            kick_res = run_action(
                binary,
                data_dir,
                "ui_kick_session",
                {"session_key": "alice:tv-2"},
                api_url=api_url,
            )
            assert "sess_alice_tv2" in kicked_keys, kicked_keys
            assert "12h" in kick_res["notice"], kick_res["notice"]

            # 4e. Detail-Page Prefetch over Unix Socket `/prefetch` (Series item_id -> resolves S01E01 without adding active streams!)
            pre_notif_count = len(sent_notifications)
            code, pre_dec = unix_http_request(
                sock_path,
                "POST",
                "/prefetch",
                {
                    "user_id": 10,
                    "item_id": "series_demo_tv",
                    "user_agent": "Infuse-Direct/8.0",
                    "device_id": "tv-2",
                },
            )
            assert code == 200 and pre_dec["status"] in ("pass", "redirect"), pre_dec
            time.sleep(0.15)
            # Prefetch must NEVER send playback notifications, while real playback & risk interception DO notify!
            assert any("风控拦截" in n.get("title", "") for n in sent_notifications), sent_notifications
            assert any("开始播放" in n.get("title", "") for n in sent_notifications), sent_notifications
            assert not any("Demo.TV.S01E01" in n.get("title", "") for n in sent_notifications[pre_notif_count:]), sent_notifications
            assert not any("用户：guest" in n.get("content", "") for n in sent_notifications), sent_notifications

            # 4f. Scheduled `tick` auto-disables expired_bob (user_id=11) via `PUT /api/media-service/users/11/enabled`
            tick_res = run_action(binary, data_dir, "tick", {}, api_url=api_url)
            assert tick_res["disabled_expired"] == 1, tick_res
            assert ("11", False) in disabled_users, disabled_users
        finally:
            daemon.terminate()
            daemon.wait(timeout=5)
            assert not os.path.exists(sock_path), "Unix socket should be cleaned up on SIGTERM"

        print("ALL MEDINEXT v2.0.0 WORKFLOW TESTS PASSED!")
    finally:
        httpd.shutdown()
        shutil.rmtree(tmp_dir, ignore_errors=True)


if __name__ == "__main__":
    main()
