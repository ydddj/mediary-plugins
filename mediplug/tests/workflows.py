#!/usr/bin/env python3
"""End-to-end integration tests for MediPlug v1.0.0 against Mock Mediary v3.4.6 + Mock GitHub Releases."""

import base64
import hashlib
import http.server
import io
import json
import os
import shutil
import socket
import sqlite3
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
import urllib.request
from urllib.parse import unquote, urlsplit


def find_free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def make_plugin_tgz(
    plugin_id: str,
    name: str,
    version: str,
    scopes: list[str],
    *,
    include_forbidden_config: bool = False,
    executable_bin: bool = True,
) -> tuple[bytes, str]:
    buf = io.BytesIO()
    manifest = {
        "api_version": 1,
        "id": plugin_id,
        "name": name,
        "version": version,
        "min_mediary_version": "3.4.6",
        "description": f"{name} Automated Test Package",
        "requested_scopes": scopes,
    }
    manifest_bytes = json.dumps(manifest, ensure_ascii=False, indent=2).encode("utf-8")
    bin_bytes = f"#!/bin/sh\necho '{plugin_id}:{version}'\n".encode("utf-8")

    with tarfile.open(fileobj=buf, mode="w:gz") as tf:
        ti_json = tarfile.TarInfo("plugin.json")
        ti_json.size = len(manifest_bytes)
        ti_json.mode = 0o644
        tf.addfile(ti_json, io.BytesIO(manifest_bytes))

        ti_bin = tarfile.TarInfo("plugin")
        ti_bin.size = len(bin_bytes)
        ti_bin.mode = 0o755 if executable_bin else 0o644
        tf.addfile(ti_bin, io.BytesIO(bin_bytes))

        if include_forbidden_config:
            bad_cfg = b'{"enabled":true}'
            ti_bad = tarfile.TarInfo("config.json")
            ti_bad.size = len(bad_cfg)
            ti_bad.mode = 0o644
            tf.addfile(ti_bad, io.BytesIO(bad_cfg))

            ti_data = tarfile.TarInfo("data/state.db")
            ti_data.size = 4
            ti_data.mode = 0o644
            tf.addfile(ti_data, io.BytesIO(b"test"))

    raw = buf.getvalue()
    sha = hashlib.sha256(raw).hexdigest()
    return raw, sha


def run_action(
    binary: str,
    data_dir: str,
    action: str,
    payload: dict,
    api_url: str,
    gh_api_url: str,
    gh_raw_url: str,
    plugin_token: str = "mp1_scoped_plugin_token",
    settings: dict | None = None,
    expect_err: bool = False,
):
    env = {
        "PATH": "/usr/local/bin:/usr/bin:/bin",
        "MEDIARY_PLUGIN_ID": "mediplug",
        "MEDIARY_PLUGIN_DATA_DIR": data_dir,
        "MEDIARY_PLUGIN_ACTION": action,
        "MEDIARY_PLUGIN_TRIGGER": "manual",
        "MEDIARY_PLUGIN_API_URL": api_url,
        "MEDIARY_PLUGIN_TOKEN": plugin_token,
        "MEDIPLUG_GITHUB_API_URL": gh_api_url,
        "MEDIPLUG_GITHUB_RAW_URL": gh_raw_url,
        "MEDIARY_PLUGIN_SETTINGS_JSON": json.dumps(
            settings
            if settings is not None
            else {
                # Mediary Web UI strips "$enabled" from settings JSON (stores "enabled" at top-level of config.json)
                "auto_sync_releases": True,
                "auto_register_bridge": True,
                "notify": True,
            }
        ),
    }
    proc = subprocess.run(
        [binary],
        input=json.dumps(payload).encode("utf-8"),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
        timeout=30,
    )
    if expect_err:
        assert proc.returncode != 0, f"Expected {action} to fail, got stdout={proc.stdout.decode()}"
        return proc.stderr.decode("utf-8")
    if proc.returncode != 0:
        raise RuntimeError(f"Action {action} failed ({proc.returncode}): {proc.stderr.decode()}")
    return json.loads(proc.stdout.decode("utf-8").strip())


def main():
    binary = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else "./plugin")
    assert os.path.isfile(binary) and os.access(binary, os.X_OK), f"Binary not executable: {binary}"

    tmp_dir = tempfile.mkdtemp(prefix="mediplug_v1_test_")
    gh_port = find_free_port()
    host_port = find_free_port()
    daemon_port = find_free_port()

    gh_base = f"http://127.0.0.1:{gh_port}"
    api_url = f"http://127.0.0.1:{host_port}/api"
    admin_secret = "adm_secret_token_346"

    # Build sample release tarballs
    mw_amd64_bytes, mw_amd64_sha = make_plugin_tgz(
        "mediwash",
        "MediWash 归档入库与权重洗版",
        "3.1.3",
        ["media-library:read", "open115:read", "open115:write", "notifications:send"],
    )
    mw_arm64_bytes, mw_arm64_sha = make_plugin_tgz(
        "mediwash",
        "MediWash 归档入库与权重洗版",
        "3.1.3",
        ["media-library:read", "open115:read", "open115:write", "notifications:send"],
    )
    mw_v314_bytes, mw_v314_sha = make_plugin_tgz(
        "mediwash",
        "MediWash 归档入库与权重洗版",
        "3.1.4",
        ["media-library:read", "open115:read", "open115:write", "notifications:send"],
    )
    mf_amd64_bytes, mf_amd64_sha = make_plugin_tgz(
        "medifind",
        "MediFind 网盘聚合引擎",
        "3.2.0",
        ["catalog:read", "open115:read", "open115:write"],
    )
    mn_amd64_bytes, mn_amd64_sha = make_plugin_tgz(
        "medinext",
        "MediNext 302 播放负载与风控",
        "2.1.1",
        ["media-service:read", "media-service:write", "open115:read"],
    )
    # Official namespace collision plugin (`emby-sync` already exists in official catalog!)
    dup_bytes, _ = make_plugin_tgz("emby-sync", "Fake Official Collision", "9.9.9", ["settings:read"])
    bad_tgz_bytes, _ = make_plugin_tgz(
        "badplug", "Bad Runtime Package", "0.1.0", ["settings:read"], include_forbidden_config=True
    )

    gh_state = {
        "mediwash_ver": "3.1.3",
        "downloads": 0,
    }

    class MockGitHub(http.server.BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def reply_json(self, code: int, obj):
            raw = json.dumps(obj).encode("utf-8")
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)

        def reply_bytes(self, code: int, data: bytes, ctype: str = "application/octet-stream"):
            self.send_response(code)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self):
            p = urlsplit(self.path).path
            if p == "/api/repos/ydddj/mediwash":
                return self.reply_json(
                    200,
                    {
                        "name": "mediwash",
                        "default_branch": "main",
                        "description": "115 自动归档洗版引擎",
                        "license": {"spdx_id": "MIT"},
                    },
                )
            if p == "/api/repos/ydddj/mediwash/releases/latest":
                ver = gh_state["mediwash_ver"]
                if ver == "3.1.3":
                    return self.reply_json(
                        200,
                        {
                            "tag_name": "v3.1.3",
                            "html_url": "https://github.com/ydddj/mediwash/releases/tag/v3.1.3",
                            "draft": False,
                            "prerelease": False,
                            "body": "Release v3.1.3 without catalog.json",
                            "assets": [
                                {
                                    "name": "mediwash-3.1.3-linux-amd64.tar.gz",
                                    "size": len(mw_amd64_bytes),
                                    "updated_at": "2026-11-27T01:00:00Z",
                                    "browser_download_url": f"{gh_base}/dl/mediwash-3.1.3-linux-amd64.tar.gz",
                                },
                                {
                                    "name": "mediwash-3.1.3-linux-arm64.tar.gz",
                                    "size": len(mw_arm64_bytes),
                                    "updated_at": "2026-11-27T01:00:00Z",
                                    "browser_download_url": f"{gh_base}/dl/mediwash-3.1.3-linux-arm64.tar.gz",
                                },
                                {
                                    "name": "SHA256SUMS",
                                    "size": 200,
                                    "updated_at": "2026-11-27T01:00:00Z",
                                    "browser_download_url": f"{gh_base}/dl/mediwash-3.1.3.SHA256SUMS",
                                },
                            ],
                        },
                    )
                else:
                    return self.reply_json(
                        200,
                        {
                            "tag_name": "v3.1.4",
                            "html_url": "https://github.com/ydddj/mediwash/releases/tag/v3.1.4",
                            "draft": False,
                            "prerelease": False,
                            "body": "Hot upgrade release v3.1.4",
                            "assets": [
                                {
                                    "name": "mediwash-3.1.4-linux-amd64.tar.gz",
                                    "size": len(mw_v314_bytes),
                                    "digest": f"sha256:{mw_v314_sha}",
                                    "updated_at": "2026-11-27T05:00:00Z",
                                    "browser_download_url": f"{gh_base}/dl/mediwash-3.1.4-linux-amd64.tar.gz",
                                }
                            ],
                        },
                    )

            # Multi-plugin release repo WITHOUT SHA256SUMS or digest (tests streaming tar.gz extraction!)
            if p == "/api/repos/ydddj/mediary-suite":
                return self.reply_json(
                    200,
                    {"name": "mediary-suite", "default_branch": "main", "license": {"spdx_id": "Apache-2.0"}},
                )
            if p == "/api/repos/ydddj/mediary-suite/releases/latest":
                return self.reply_json(
                    200,
                    {
                        "tag_name": "v2026.11",
                        "draft": False,
                        "assets": [
                            {
                                "name": "medifind-3.2.0-linux-amd64.tar.gz",
                                "size": len(mf_amd64_bytes),
                                "updated_at": "2026-11-27T02:00:00Z",
                                "browser_download_url": f"{gh_base}/dl/medifind-3.2.0-linux-amd64.tar.gz",
                            },
                            {
                                "name": "medinext-2.1.1-linux-amd64.tar.gz",
                                "size": len(mn_amd64_bytes),
                                "updated_at": "2026-11-27T02:00:00Z",
                                "browser_download_url": f"{gh_base}/dl/medinext-2.1.1-linux-amd64.tar.gz",
                            },
                            {
                                "name": "emby-sync-9.9.9-linux-amd64.tar.gz",
                                "size": len(dup_bytes),
                                "updated_at": "2026-11-27T02:00:00Z",
                                "browser_download_url": f"{gh_base}/dl/emby-sync-9.9.9-linux-amd64.tar.gz",
                            },
                        ],
                    },
                )

            if p == "/dl/mediwash-3.1.3.SHA256SUMS":
                sums = (
                    f"{mw_amd64_sha}  mediwash-3.1.3-linux-amd64.tar.gz\n"
                    f"{mw_arm64_sha}  mediwash-3.1.3-linux-arm64.tar.gz\n"
                ).encode("utf-8")
                return self.reply_bytes(200, sums, "text/plain")
            if p == "/dl/mediwash-3.1.3-linux-amd64.tar.gz":
                gh_state["downloads"] += 1
                return self.reply_bytes(200, mw_amd64_bytes)
            if p == "/dl/mediwash-3.1.3-linux-arm64.tar.gz":
                gh_state["downloads"] += 1
                return self.reply_bytes(200, mw_arm64_bytes)
            if p == "/dl/mediwash-3.1.4-linux-amd64.tar.gz":
                gh_state["downloads"] += 1
                return self.reply_bytes(200, mw_v314_bytes)
            if p == "/dl/medifind-3.2.0-linux-amd64.tar.gz":
                return self.reply_bytes(200, mf_amd64_bytes)
            if p == "/dl/medinext-2.1.1-linux-amd64.tar.gz":
                return self.reply_bytes(200, mn_amd64_bytes)
            if p == "/dl/emby-sync-9.9.9-linux-amd64.tar.gz":
                return self.reply_bytes(200, dup_bytes)

            if p == "/raw/demo/direct-store/main/catalog.json":
                return self.reply_json(
                    200,
                    {
                        "schema_version": 1,
                        "plugins": [
                            {
                                "id": "demo-direct",
                                "name": "Direct Store Plugin",
                                "version": "1.0.0",
                                "description": "Direct catalog.json plugin",
                                "author": "demo",
                                "homepage": "https://github.com/demo/direct-store",
                                "source": "https://github.com/demo/direct-store",
                                "license": "MIT",
                                "api_version": 1,
                                "permissions": ["settings:read"],
                                "artifacts": {
                                    "linux-amd64": {
                                        "url": f"{gh_base}/dl/medifind-3.2.0-linux-amd64.tar.gz",
                                        "sha256": mf_amd64_sha,
                                        "size": len(mf_amd64_bytes),
                                    }
                                },
                            }
                        ],
                    },
                )

            return self.reply_json(404, {"error": "not found"})

    # Stateful Mock Mediary v3.4.6 Host (`plugin_catalog_sources` + `/api/plugin-store` aggregator)
    host_state = {
        "sources": [
            {
                "id": "official",
                "name": "Mediary 官方插件源",
                "url": "https://raw.githubusercontent.com/KyleYu2024/Mediary-Plugins/main/catalog.json",
                "enabled": True,
                "builtin": True,
            }
        ],
        "installed": {
            "mediplug": {"id": "mediplug", "version": "1.0.0", "enabled": True},
        },
        "notifications": [],
    }

    class MockMediary346(http.server.BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def reply(self, code: int, obj):
            raw = json.dumps(obj).encode("utf-8")
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)

        def require_admin(self) -> bool:
            auth = self.headers.get("Authorization", "")
            if auth != f"Bearer {admin_secret}":
                self.reply(401, {"error": "admin api_token required"})
                return False
            return True

        def require_plugin_or_admin(self) -> bool:
            auth = self.headers.get("Authorization", "")
            if auth in (f"Bearer {admin_secret}", "Bearer mp1_scoped_plugin_token"):
                return True
            self.reply(401, {"error": "unauthorized"})
            return False

        def do_GET(self):
            p = urlsplit(self.path).path
            if p == "/api/settings":
                if not self.require_plugin_or_admin():
                    return
                return self.reply(200, {"tmdb_proxy_url": ""})

            if p == "/api/plugin/plugins":
                if not self.require_plugin_or_admin():
                    return
                return self.reply(200, {"plugins": list(host_state["installed"].values())})

            if p == "/api/plugin-store/sources":
                if not self.require_admin():
                    return
                return self.reply(200, {"sources": host_state["sources"]})

            if p == "/api/plugin-store":
                if not self.require_admin():
                    return
                merged = [
                    {
                        "id": "emby-sync",
                        "name": "Emby 官方同步助手",
                        "version": "1.2.0",
                        "description": "Official built-in plugin",
                        "author": "Mediary",
                        "source_id": "official",
                        "source_name": "Mediary 官方插件源",
                        "official": True,
                        "installed": False,
                        "update_available": False,
                        "permissions": ["settings:read"],
                        "artifacts": {},
                    }
                ]
                seen_ids = {"emby-sync": "official"}
                conflicts = []

                for src in host_state["sources"]:
                    if src.get("builtin") or not src.get("enabled", True):
                        continue
                    with urllib.request.urlopen(src["url"], timeout=5) as resp:
                        doc = json.loads(resp.read().decode("utf-8"))
                    assert doc["schema_version"] == 1, f"Invalid schema_version in {src['url']}: {doc}"
                    for plug in doc.get("plugins", []):
                        pid = plug["id"]
                        # Verify strict v3.4.6 artifact sha256 & size fields
                        assert plug.get("artifacts"), f"Plugin {pid} missing artifacts"
                        for arch, art in plug["artifacts"].items():
                            assert len(art["sha256"]) == 64 and art["sha256"].islower(), art
                            assert int(art["size"]) > 0, art
                        if pid in seen_ids:
                            conflicts.append(
                                {
                                    "plugin_id": pid,
                                    "rejected_source": src["id"],
                                    "kept_source": seen_ids[pid],
                                    "reason": "plugin id collides with protected official or earlier source",
                                }
                            )
                            continue
                        seen_ids[pid] = src["id"]
                        inst = host_state["installed"].get(pid)
                        inst_ver = inst["version"] if inst else ""
                        merged.append(
                            {
                                **plug,
                                "source_id": src["id"],
                                "source_name": src["name"],
                                "official": False,
                                "installed": bool(inst),
                                "installed_version": inst_ver,
                                "enabled": bool(inst and inst.get("enabled")),
                                "update_available": bool(inst and inst_ver != plug["version"]),
                            }
                        )

                return self.reply(200, {"plugins": merged, "conflicts": conflicts, "sources": host_state["sources"]})

            return self.reply(404, {"error": "not found"})

        def do_POST(self):
            p = urlsplit(self.path).path
            length = int(self.headers.get("Content-Length", "0"))
            body = json.loads(self.rfile.read(length) or b"{}")

            if p == "/api/plugin/notifications":
                if not self.require_plugin_or_admin():
                    return
                host_state["notifications"].append(body)
                return self.reply(200, {"ok": True})

            if p == "/api/plugin-store/sources":
                if not self.require_admin():
                    return
                sid = body.get("id", "").strip()
                url = body.get("url", "").strip()
                if sid == "official" or not sid:
                    return self.reply(400, {"error": "invalid or protected source id"})
                if not (url.startswith("http://") or url.startswith("https://")):
                    return self.reply(400, {"error": "url must use http or https"})
                # Simulate strict INSERT-only POST returning 409 on duplicate ID!
                if any(s["id"] == sid for s in host_state["sources"]):
                    return self.reply(409, {"error": "source id already exists"})
                entry = {
                    "id": sid,
                    "name": body.get("name") or sid,
                    "url": url,
                    "enabled": bool(body.get("enabled", True)),
                    "builtin": False,
                }
                host_state["sources"].append(entry)
                return self.reply(201, entry)

            if p.startswith("/api/plugin-store/") and p.endswith("/install"):
                if not self.require_admin():
                    return
                pid = unquote(p.split("/api/plugin-store/", 1)[1].rsplit("/install", 1)[0])
                # Find plugin in enabled sources, download & verify sha256 just like Mediary 3.4.6!
                for src in host_state["sources"]:
                    if src.get("builtin") or not src.get("enabled", True):
                        continue
                    with urllib.request.urlopen(src["url"], timeout=5) as resp:
                        doc = json.loads(resp.read().decode("utf-8"))
                    for plug in doc.get("plugins", []):
                        if plug["id"] == pid and pid != "emby-sync":
                            art = plug["artifacts"]["linux-amd64"]
                            with urllib.request.urlopen(art["url"], timeout=5) as dl:
                                raw_pkg = dl.read()
                            assert len(raw_pkg) == art["size"], "Size mismatch during host install!"
                            assert hashlib.sha256(raw_pkg).hexdigest() == art["sha256"], "SHA256 mismatch!"
                            host_state["installed"][pid] = {
                                "id": pid,
                                "version": plug["version"],
                                "enabled": False,
                            }
                            return self.reply(200, {"ok": True, "id": pid, "version": plug["version"]})
                return self.reply(404, {"error": f"plugin {pid} not found in store"})

            return self.reply(404, {"error": "not found"})

        def do_DELETE(self):
            p = urlsplit(self.path).path
            if p.startswith("/api/plugin-store/sources/"):
                if not self.require_admin():
                    return
                sid = unquote(p.split("/api/plugin-store/sources/", 1)[1])
                if sid == "official":
                    return self.reply(400, {"error": "cannot delete official source"})
                before = len(host_state["sources"])
                host_state["sources"] = [s for s in host_state["sources"] if s["id"] != sid]
                if len(host_state["sources"]) == before:
                    return self.reply(404, {"error": "source not found"})
                return self.reply(200, {"ok": True})
            return self.reply(404, {"error": "not found"})

    gh_httpd = http.server.ThreadingHTTPServer(("127.0.0.1", gh_port), MockGitHub)
    host_httpd = http.server.ThreadingHTTPServer(("127.0.0.1", host_port), MockMediary346)
    threading.Thread(target=gh_httpd.serve_forever, daemon=True).start()
    threading.Thread(target=host_httpd.serve_forever, daemon=True).start()

    try:
        data_dir = os.path.join(tmp_dir, "data")
        os.makedirs(data_dir, exist_ok=True)
        with open(os.path.join(tmp_dir, "config.json"), "w", encoding="utf-8") as f:
            json.dump(
                {
                    "enabled": True,
                    "granted_scopes": ["plugins:read", "settings:read", "notifications:send"],
                    "pending_scope_approval": False,
                },
                f,
            )

        # Pre-set custom listen_port before starting `./plugin daemon`
        run_action(
            binary,
            data_dir,
            "ui_save_engine",
            {"engine": {"listen_host": "127.0.0.1", "listen_port": daemon_port, "deep_inspect_archive": True}},
            api_url,
            f"{gh_base}/api",
            f"{gh_base}/raw",
        )

        daemon_env = {
            "PATH": "/usr/local/bin:/usr/bin:/bin",
            "MEDIARY_PLUGIN_ID": "mediplug",
            "MEDIARY_PLUGIN_DATA_DIR": data_dir,
            "MEDIARY_PLUGIN_API_URL": api_url,
            "MEDIARY_PLUGIN_TOKEN": "mp1_scoped_plugin_token",
        }
        daemon = subprocess.Popen([binary, "daemon"], env=daemon_env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            for _ in range(40):
                try:
                    with urllib.request.urlopen(f"http://127.0.0.1:{daemon_port}/health", timeout=1) as r:
                        h = json.loads(r.read().decode("utf-8"))
                        if h.get("ok") and h.get("service") == "mediplug":
                            break
                except Exception:
                    time.sleep(0.05)
            else:
                raise RuntimeError("MediPlug HTTP daemon did not become ready in time")

            # 1. Verify bootstrap BEFORE Admin API Token is configured + SQLite WAL mode + dashboard.json
            boot0 = run_action(binary, data_dir, "ui_bootstrap", {}, api_url, f"{gh_base}/api", f"{gh_base}/raw")
            assert boot0["version"] == "1.0.0"
            assert boot0["host_status"]["plugin_enabled"] is True
            assert boot0["host_status"]["daemon_ready"] is True
            assert boot0["host_status"]["active_port"] == daemon_port
            assert boot0["host_status"]["store_auth_ready"] is False
            assert boot0["host_status"]["plugins_read_ready"] is True

            with sqlite3.connect(os.path.join(data_dir, "mediplug.db")) as con:
                assert con.execute("PRAGMA journal_mode").fetchone()[0].lower() == "wal"
            assert os.path.isfile(os.path.join(data_dir, "dashboard.json"))

            # 2. Configure Admin API Token -> unlocks `/api/plugin-store/sources` and masks secret!
            boot1 = run_action(
                binary,
                data_dir,
                "ui_save_engine",
                {
                    "engine": {
                        "admin_api_token": admin_secret,
                        "listen_host": "127.0.0.1",
                        "listen_port": daemon_port,
                        "deep_inspect_archive": True,
                    }
                },
                api_url,
                f"{gh_base}/api",
                f"{gh_base}/raw",
            )["bootstrap"]
            assert boot1["host_status"]["store_auth_ready"] is True, boot1["host_status"]
            assert "******" in boot1["engine"]["admin_api_token"], boot1["engine"]
            assert len(boot1["host_sources"]) == 1 and boot1["host_sources"][0]["id"] == "official"

            # Guardrail: deleting or overwriting `official` MUST fail!
            err_official = run_action(
                binary,
                data_dir,
                "ui_delete_host_source",
                {"id": "official"},
                api_url,
                f"{gh_base}/api",
                f"{gh_base}/raw",
                expect_err=True,
            )
            assert "official" in err_official

            # 3. Direct catalog.json Source Management (Add -> Toggle OFF via DELETE+POST fallback -> Delete)
            add_direct = run_action(
                binary,
                data_dir,
                "ui_save_host_source",
                {
                    "source": {
                        "id": "demo-direct-store",
                        "name": "测试直连源",
                        "url": f"{gh_base}/raw/demo/direct-store/main/catalog.json",
                        "enabled": True,
                    }
                },
                api_url,
                f"{gh_base}/api",
                f"{gh_base}/raw",
            )["bootstrap"]
            assert any(p["id"] == "demo-direct" for p in add_direct["store"]["plugins"]), add_direct["store"]

            toggled = run_action(
                binary,
                data_dir,
                "ui_toggle_host_source",
                {"id": "demo-direct-store", "enabled": False},
                api_url,
                f"{gh_base}/api",
                f"{gh_base}/raw",
            )["bootstrap"]
            assert not any(p["id"] == "demo-direct" for p in toggled["store"]["plugins"]), toggled["store"]

            deleted = run_action(
                binary,
                data_dir,
                "ui_delete_host_source",
                {"id": "demo-direct-store"},
                api_url,
                f"{gh_base}/api",
                f"{gh_base}/raw",
            )["bootstrap"]
            assert not any(s["id"] == "demo-direct-store" for s in deleted["host_sources"])

            # 4. Plain GitHub Release Repo Bridge (NO catalog.json -> auto-converted + registered in host!)
            gh_res = run_action(
                binary,
                data_dir,
                "ui_save_github_repo",
                {
                    "repo": {
                        "repo": "https://github.com/ydddj/mediwash",
                        "name": "MediWash Release 源",
                        "register_mode": "unified",
                        "enabled": True,
                    },
                    "sync_now": True,
                },
                api_url,
                f"{gh_base}/api",
                f"{gh_base}/raw",
            )
            boot_gh = gh_res["bootstrap"]
            assert boot_gh["host_status"]["bridge_registered"] is True, boot_gh["host_status"]
            mw_store = [p for p in boot_gh["store"]["plugins"] if p["id"] == "mediwash"]
            assert len(mw_store) == 1, boot_gh["store"]
            assert mw_store[0]["version"] == "3.1.3"
            assert mw_store[0]["permissions"] == [
                "media-library:read",
                "open115:read",
                "open115:write",
                "notifications:send",
            ]

            # Verify live `/catalog.json` HTTP response has both `linux-amd64` and `linux-arm64`!
            with urllib.request.urlopen(f"http://127.0.0.1:{daemon_port}/catalog.json", timeout=5) as r:
                live_cat = json.loads(r.read().decode("utf-8"))
            assert live_cat["schema_version"] == 1
            assert set(live_cat["plugins"][0]["artifacts"].keys()) == {"linux-amd64", "linux-arm64"}
            assert live_cat["plugins"][0]["artifacts"]["linux-amd64"]["sha256"] == mw_amd64_sha

            # Verify SQLite release_asset_cache avoids redundant re-downloads on second sync!
            dl_before = gh_state["downloads"]
            run_action(
                binary,
                data_dir,
                "ui_sync_github_repo",
                {"id": "gh-ydddj-mediwash"},
                api_url,
                f"{gh_base}/api",
                f"{gh_base}/raw",
            )
            assert gh_state["downloads"] == dl_before, "Cached release assets must not be re-downloaded!"

            # 5. Multi-plugin dedicated GitHub repo + Official Namespace Collision Detection (`conflicts`)
            suite_boot = run_action(
                binary,
                data_dir,
                "ui_save_github_repo",
                {
                    "repo": {
                        "id": "suite-dedicated",
                        "repo": "ydddj/mediary-suite",
                        "name": "全栈套件独立源",
                        "register_mode": "dedicated",
                        "enabled": True,
                    },
                    "sync_now": True,
                },
                api_url,
                f"{gh_base}/api",
                f"{gh_base}/raw",
            )["bootstrap"]
            store_ids = {p["id"] for p in suite_boot["store"]["plugins"]}
            assert {"medifind", "medinext", "mediwash", "emby-sync"} <= store_ids, store_ids
            # Official `emby-sync` kept at v1.2.0 while rogue `emby-sync@9.9.9` is recorded in `conflicts`!
            emby_entry = [p for p in suite_boot["store"]["plugins"] if p["id"] == "emby-sync"][0]
            assert emby_entry["official"] is True and emby_entry["version"] == "1.2.0"
            assert any(c["plugin_id"] == "emby-sync" for c in suite_boot["store"]["conflicts"]), suite_boot["store"]["conflicts"]

            # 6. Install `mediwash` v3.1.3 via Host Store Installer, then publish v3.1.4 & verify `tick` + Update Available + Notification!
            inst_boot = run_action(
                binary,
                data_dir,
                "ui_install_store_plugin",
                {"plugin_id": "mediwash"},
                api_url,
                f"{gh_base}/api",
                f"{gh_base}/raw",
            )["bootstrap"]
            mw_after_inst = [p for p in inst_boot["store"]["plugins"] if p["id"] == "mediwash"][0]
            assert mw_after_inst["installed"] is True and mw_after_inst["update_available"] is False

            gh_state["mediwash_ver"] = "3.1.4"
            tick_res = run_action(binary, data_dir, "tick", {"manual": True}, api_url, f"{gh_base}/api", f"{gh_base}/raw")
            assert tick_res["ok"] is True
            assert any("3.1.3 -> 3.1.4" in u for u in tick_res["upgraded"]), tick_res
            assert any("新版本" in n.get("title", "") for n in host_state["notifications"]), host_state["notifications"]

            hc_res = run_action(binary, data_dir, "health_check", {}, api_url, f"{gh_base}/api", f"{gh_base}/raw")
            assert hc_res["updates_available"] == 1, hc_res

            # 7. Package Validator (`ui_inspect_archive`): valid package passes, `config.json`/`data/` package blocked!
            good_b64 = base64.b64encode(mn_amd64_bytes).decode("ascii")
            good_ins = run_action(
                binary,
                data_dir,
                "ui_inspect_archive",
                {
                    "filename": "medinext-2.1.1-linux-amd64.tar.gz",
                    "file_base64": f"data:application/gzip;base64,{good_b64}",
                    "add_to_hosted": True,
                },
                api_url,
                f"{gh_base}/api",
                f"{gh_base}/raw",
            )
            assert good_ins["inspection"]["valid_for_mediary_346"] is True
            assert good_ins["inspection"]["sha256"] == mn_amd64_sha

            bad_b64 = base64.b64encode(bad_tgz_bytes).decode("ascii")
            bad_ins = run_action(
                binary,
                data_dir,
                "ui_inspect_archive",
                {
                    "filename": "badplug-0.1.0-linux-amd64.tar.gz",
                    "file_base64": bad_b64,
                    "add_to_hosted": False,
                },
                api_url,
                f"{gh_base}/api",
                f"{gh_base}/raw",
            )
            assert bad_ins["inspection"]["valid_for_mediary_346"] is False
            assert "config.json" in bad_ins["inspection"]["forbidden_entries"]

            # Trying to host an invalid archive must fail cleanly
            run_action(
                binary,
                data_dir,
                "ui_inspect_archive",
                {
                    "filename": "badplug-0.1.0-linux-amd64.tar.gz",
                    "file_base64": bad_b64,
                    "add_to_hosted": True,
                },
                api_url,
                f"{gh_base}/api",
                f"{gh_base}/raw",
                expect_err=True,
            )

            # Manual test notification
            nt = run_action(binary, data_dir, "ui_test_notify", {}, api_url, f"{gh_base}/api", f"{gh_base}/raw")
            assert nt["ok"] is True

        finally:
            daemon.terminate()
            daemon.wait(timeout=5)
            assert not os.path.exists(os.path.join(data_dir, "daemon_state.json")), "daemon_state.json must be cleaned up on SIGTERM"

        print("ALL MEDIPLUG v1.0.0 WORKFLOW TESTS PASSED!")
    finally:
        gh_httpd.shutdown()
        host_httpd.shutdown()
        shutil.rmtree(tmp_dir, ignore_errors=True)


if __name__ == "__main__":
    main()
