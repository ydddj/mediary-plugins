"""End-to-end integration test suite for MediWash v2.0.0 against a stateful Mock Mediary 3.3.2+ host."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlsplit
import json
import os
import sqlite3
import subprocess
import sys
import tempfile
import threading

STATE = {
    "writes": [],
    "flowlink_triggers": 0,
    "cloud_dirs": {
        "100": [
            {"file_id": "110", "parent_id": "100", "name": "热辣滚烫 (2024)", "is_dir": True},
            {"file_id": "130", "parent_id": "100", "name": "link", "is_dir": True},
            {"file_id": "140", "parent_id": "100", "name": "保留压缩包目录", "is_dir": True},
            {"file_id": "101", "parent_id": "100", "name": "Alpha.2020.2160p.HDR10.WEB-DL.H265.DDP5.1.mkv", "size": 12_000_000_000, "pick_code": "pc_up", "sha1": "SHA_UPGRADE", "is_dir": False},
            {"file_id": "102", "parent_id": "100", "name": "Alpha.2020.2160p.HDR10.WEB-DL.H265.DDP5.1.chs.ass", "size": 120_000, "pick_code": "pc_sub", "sha1": "SHA_SUB", "is_dir": False},
            {"file_id": "103", "parent_id": "100", "name": "Alpha.2020.2160p.HDR10.LOW.mkv", "size": 1_200_000_000, "pick_code": "pc_low", "sha1": "SHA_LOW", "is_dir": False},
            {"file_id": "104", "parent_id": "100", "name": "UnknownMysteryFilm.2026.1080p.WEB-DL.mkv", "size": 3_000_000_000, "pick_code": "pc_unk", "sha1": "SHA_UNK", "is_dir": False},
        ],
        "110": [
            {"file_id": "111", "parent_id": "110", "name": "热辣滚烫.2024.2160p.WEB-DL.H265.mkv", "size": 9_000_000_000, "pick_code": "pc_new", "sha1": "SHA_NEW", "is_dir": False},
            {"file_id": "112", "parent_id": "110", "name": "热辣滚烫.nfo", "size": 4_096, "pick_code": "pc_nfo", "sha1": "", "is_dir": False},
            {"file_id": "113", "parent_id": "110", "name": "poster.jpg", "size": 250_000, "pick_code": "pc_jpg", "sha1": "", "is_dir": False},
            {"file_id": "114", "parent_id": "110", "name": "免责声明.txt", "size": 512, "pick_code": "pc_txt", "sha1": "", "is_dir": False},
        ],
        "130": [
            {"file_id": "131", "parent_id": "130", "name": "电视剧", "is_dir": True},
        ],
        "131": [
            {"file_id": "132", "parent_id": "131", "name": "Season 01", "is_dir": True},
        ],
        "132": [],
        "140": [
            {"file_id": "141", "parent_id": "140", "name": "archive.zip", "size": 500_000_000, "pick_code": "pc_zip", "sha1": "", "is_dir": False},
        ],
        "200": [],  # archived library bucket in mock
    },
    "library_arrived_115": False,
    "pt_download": {"status": "downloading"},
    "deleted_sources": {},
    "recycled_115": set(),
    "organize_calls": [],
    "omit_organize_strm_paths": False,
    "extra_items": {},
}

CATEGORIES_YAML = """
movie:
  电影/技术专区/蓝光原盘:
    genre_ids: "-16"
    file_extension: ".iso,iso,.ISO,ISO"
  电影/华语电影/2020-2030:
    year: "2020-2030"
tv:
  动漫/儿童:
    genre_ids: "16"
  动漫/国漫:
    genre_ids: "16"
"""


def mk_source(sid, name, cat, mbps, file_id="", sha1="", cloud_ext=""):
    return {
        "id": sid,
        "name": name,
        "strm_path": f"/strm/cloud/媒体库/{cat}/{sid}.strm",
        "cloud_path": f"媒体库/{cat}/{name}{cloud_ext}",
        "size": mbps * 350_000_000,
        "bitrate": mbps * 1_000_000,
        "file_id": file_id,
        "sha1": sha1,
    }


def mk_stream(sid, mbps, width=3840, hdr="HDR10"):
    return {
        "media_source_id": sid,
        "stream_type": "video",
        "codec": "hevc",
        "width": width,
        "height": 2160 if width >= 3800 else 720,
        "hdr_format": hdr,
        "frame_rate": "24000/1001",
        "bit_depth": 10,
        "bitrate": mbps * 1_000_000,
    }


class MockMediary(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def reply(self, payload, status=200):
        raw = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def live_sources(self, item_id, base_list):
        removed = STATE["deleted_sources"].get(item_id, set())
        return [s for s in base_list if s["id"] not in removed]

    def build_items(self):
        alpha_sources = [
            mk_source("s_mid", "Alpha.2020.2160p.HDR10.WEB-DL.mkv", "电影/华语电影/2020-2030", 12),
            mk_source("s_low_dup", "Alpha.2020.2160p.HDR10.WEB-DL.mkv", "电影/华语电影/2020-2030", 4),
            # Same movie also stored in Blu-ray ISO zone -> must NEVER cross-contaminate!
            mk_source("s_iso", "Alpha.2020.2160p.BluRay", "电影/技术专区/蓝光原盘", 48, cloud_ext=".iso"),
        ]
        alpha_streams = [
            mk_stream("s_mid", 12),
            mk_stream("s_low_dup", 4),
            mk_stream("s_iso", 48),
        ]
        if STATE["library_arrived_115"]:
            alpha_sources.append(
                mk_source(
                    "s_new_115",
                    "Alpha.2020.2160p.HDR10.WEB-DL.H265.DDP5.1.mkv",
                    "电影/华语电影/2020-2030",
                    32,
                    file_id="101",
                    sha1="SHA_UPGRADE",
                )
            )
            alpha_streams.append(mk_stream("s_new_115", 32))

        pt_sources = [
            mk_source("pt_old", "1917.2019.720p.SDR.HDTV.H264.mp4", "电影/华语电影/2020-2030", 2),
        ]
        pt_streams = [mk_stream("pt_old", 2, width=1280, hdr="SDR")]
        if STATE["pt_download"]["status"] == "completed":
            pt_sources.append(
                mk_source(
                    "pt_new",
                    "1917.2019.2160p.SDR.UHD.Blu-Ray.Remux.HEVC.TrueHD.Dolby.Atmos.7.1-CHD.mkv",
                    "电影/华语电影/2020-2030",
                    45,
                )
            )
            pt_streams.append(mk_stream("pt_new", 45, width=3840, hdr="SDR"))

        single_sources = [
            mk_source("single_1", "Single.2022.1080p.WEB-DL.mkv", "电影/华语电影/2020-2030", 8),
        ]

        return {
            "film_alpha": {
                "id": "film_alpha",
                "library_id": "lib1",
                "name": "Alpha",
                "item_type": "movie",
                "tmdb_id": "101",
                "sources": self.live_sources("film_alpha", alpha_sources),
                "streams": alpha_streams,
            },
            "film_1917": {
                "id": "film_1917",
                "library_id": "lib1",
                "name": "一九一七",
                "item_type": "movie",
                "tmdb_id": "530915",
                "sources": self.live_sources("film_1917", pt_sources),
                "streams": pt_streams,
            },
            "film_single": {
                "id": "film_single",
                "library_id": "lib1",
                "name": "SingleFilm",
                "item_type": "movie",
                "tmdb_id": "999",
                "sources": single_sources,
                "streams": [mk_stream("single_1", 8, width=1920, hdr="SDR")],
            },
            "ep_river_5": {
                "id": "ep_river_5",
                "library_id": "lib_tv",
                "name": "假扮戏子",
                "item_type": "episode",
                "tmdb_id": "9999005",
                "series_tmdb_id": "203483",
                "season": 1,
                "episode": 5,
                "sources": [
                    {
                        "id": "ep_src_1",
                        "name": "两京十五日.2026.S01E05.2160p.WEB-DL.SDR.strm",
                        "strm_path": "/strm/cloud/媒体库/动漫/国漫/两京十五日.strm",
                        "cloud_path": "",
                        "size": 1_445_007_245,
                        "bitrate": 4_432_175,
                        "file_id": "",
                        "sha1": "",
                    }
                ],
                "streams": [mk_stream("ep_src_1", 12, width=3840, hdr="HDR10")],
            },
            **STATE.get("extra_items", {}),
        }

    def do_GET(self):
        parsed = urlsplit(self.path)
        path = parsed.path
        q = parse_qs(parsed.query)
        items_map = self.build_items()

        if path == "/api/settings":
            return self.reply({
                "library_paths": {
                    "movie_rename_format": "{{title}} ({{year}}) [tmdbid-{{tmdbid}}]/{{title}}.{{videoFormat}}{{fileExt}}",
                    "tv_rename_format": "{{title}} [tmdbid-{{tmdbid}}]/Season {{season}}/{{title}}.{{season_episode}}{{fileExt}}",
                },
                "flowlink": {"task_name": "115-895"},
                "flowlink_enabled": True,
            })
        if path == "/api/settings/category-file":
            return self.reply({"content": CATEGORIES_YAML})
        if path == "/api/filter/rules":
            return self.reply({"items": [{"keyword": "CAM抢先版", "action": "exclude", "enabled": True}]})
        if path == "/api/open115/status":
            return self.reply({"status": "connected"})
        if path == "/api/open115/config":
            return self.reply({
                "target_folder": "最近接收",
                "target_cid": "200",
                "offline_folder": "云下载",
                "offline_folder_cid": "201",
                "transfer_source_folder": "转存监控",
                "transfer_source_cid": "205",
                "transfer_target_folder": "媒体库",
                "transfer_target_cid": "900",
                "transfer_tasks": [
                    {
                        "name": "115-895",
                        "is_default": True,
                        "enabled": False,
                        "source_path": "转存监控",
                        "source_cid": "205",
                        "target_dir": "媒体库",
                        "target_cid": "900",
                        "movie_template": "{{title}} ({{year}}) [tmdbid-{{tmdbid}}]/{{title}}{{fileExt}}",
                        "tv_template": "{{title}} ({{year}}) [tmdbid-{{tmdbid}}]/Season {{season}}/{{title}}.{{season_episode}}{{fileExt}}",
                    }
                ],
            })
        if path == "/api/open115/web-cookie/status":
            return self.reply({"status": "connected"})
        if path == "/api/plugin/sites":
            return self.reply({"items": [{"id": 1, "name": "MTeam", "enabled": True}]})
        if path == "/api/search/tmdb":
            return self.reply({"items": [{"id": 88801, "title": "神秘新片", "media_type": "movie", "release_date": "2026-05-01"}]})
        if path == "/api/search/tmdb/details":
            tid = int(q.get("tmdb_id", ["0"])[0])
            if tid == 122079:
                return self.reply({
                    "data": {
                        "id": 122079,
                        "name": "铠甲勇士刑天",
                        "first_air_date": "2011-01-17",
                        "seasons": [
                            {"season_number": 0, "episode_count": 4},
                            {"season_number": 1, "episode_count": 30},
                            {"season_number": 2, "episode_count": 30},
                        ],
                    }
                })
            return self.reply({"data": {"id": tid, "title": "神秘新片", "release_date": "2026-05-01", "backdrop_path": "/mock_backdrop.jpg"}})

        if path == "/api/plugin/open115/files":
            cid = q.get("cid", ["0"])[0]
            offset = int(q.get("offset", ["0"])[0])
            limit = int(q.get("limit", ["200"])[0])
            folder_items = STATE["cloud_dirs"].get(cid, [])
            sliced = folder_items[offset : offset + limit]
            return self.reply({
                "cid": cid,
                "offset": offset,
                "limit": limit,
                "count": len(folder_items),
                "auth_source": "openapi",
                "items": sliced,
            })

        if path == "/api/plugin/library/items":
            tmdb = q.get("tmdb_id", [""])[0]
            kw = q.get("search", [""])[0]
            media_type = q.get("media_type", [""])[0]
            multi_only = q.get("has_multiple_versions", [""])[0] == "true"
            hdr_fmt = q.get("hdr_format", [""])[0].lower()
            src_kind = q.get("source_kind", [""])[0].lower()
            all_list = list(items_map.values())
            if media_type == "movie":
                all_list = [x for x in all_list if x.get("item_type") == "movie"]
            elif media_type == "tv":
                all_list = [x for x in all_list if x.get("item_type") == "episode"]
            if tmdb:
                filtered = [
                    x for x in all_list
                    if x.get("tmdb_id") == tmdb or x.get("series_tmdb_id") == tmdb
                ]
            elif kw:
                filtered = [x for x in all_list if kw.lower() in x["name"].lower()]
            else:
                filtered = all_list
            if multi_only:
                filtered = [x for x in filtered if len(x.get("sources", [])) > 1]
            if hdr_fmt:
                filtered = [
                    x for x in filtered
                    if any(hdr_fmt in st.get("hdr_format", "").lower() for st in x.get("streams", []))
                ]
            if src_kind == "iso":
                filtered = [
                    x for x in filtered
                    if any(s.get("cloud_path", "").lower().endswith(".iso") for s in x.get("sources", []))
                ]
            start = int(q.get("start", ["0"])[0])
            limit = int(q.get("limit", ["50"])[0])
            return self.reply({
                "start": start,
                "total": len(filtered),
                "items": filtered[start : start + limit],
            })

        if path.startswith("/api/plugin/library/items/"):
            iid = path.rsplit("/", 1)[-1]
            if iid in items_map:
                return self.reply({"item": items_map[iid]})
            return self.reply({"error": "not found"}, 404)

        if path == "/api/plugin/torrents":
            return self.reply({
                "items": [
                    {
                        "title": "1917.2019.2160p.SDR.UHD.Blu-Ray.Remux.HEVC.TrueHD.Dolby.Atmos.7.1-CHD.mkv",
                        "site_id": 1,
                        "site_name": "MTeam",
                        "seeders": 18,
                        "size": 52_000_000_000,
                        "downloadvolumefactor": 0.0,
                        "hit_and_run": False,
                        "download_url": "https://pt.example/1917.torrent",
                    },
                    {
                        "title": "1917.2019.2160p.Blu-Ray.PAID.mkv",
                        "site_id": 1,
                        "site_name": "MTeam",
                        "seeders": 50,
                        "size": 40_000_000_000,
                        "downloadvolumefactor": 1.0,
                        "hit_and_run": False,
                        "download_url": "https://pt.example/paid.torrent",
                    },
                ]
            })

        if path == "/api/plugin/media-downloads/77":
            return self.reply(STATE["pt_download"])

        return self.reply({}, 404)

    def do_POST(self):
        self.handle_mutating()

    def do_DELETE(self):
        self.handle_mutating()

    def handle_mutating(self):
        length = int(self.headers.get("Content-Length", "0"))
        body = json.loads(self.rfile.read(length)) if length > 0 else {}
        path = urlsplit(self.path).path

        if path == "/api/tmdb/resolve":
            title = body.get("title", "")
            if title == "Alpha":
                return self.reply({"data": {"tmdb_id": "101"}})
            if title == "热辣滚烫":
                return self.reply({"data": {"tmdb_id": "1075794"}})
            if title == "一九一七":
                return self.reply({"data": {"tmdb_id": "530915"}})
            if title == "两京十五日":
                return self.reply({"data": {"tmdb_id": "203483"}})
            return self.reply({"error": "not found"}, 404)

        if path == "/api/plugin/open115/media-info":
            pc = body.get("pick_code", "")
            br_map = {"pc_up": 32, "pc_low": 2, "pc_new": 18, "pc_unk": 8}
            mbps = br_map.get(pc, 10)
            return self.reply({
                "status": "ready",
                "cached": True,
                "bitrate": mbps * 1_000_000,
                "streams": [
                    mk_stream("", mbps),
                    {"stream_type": "audio", "codec": "eac3", "channels": 6, "title": "DDP 5.1"},
                ],
            })

        STATE["writes"].append((path, self.command, body))

        if path == "/api/plugin/open115/files/delete":
            parent_cid = body.get("parent_cid", "")
            if not parent_cid or parent_cid == "0" or not str(parent_cid).isdigit():
                return self.reply({"ok": False, "error": "parent_cid 为必填项且必须是有效的纯数字目录 ID"}, 400)
            del_ids = set(body.get("file_ids", []))
            assert 1 <= len(del_ids) <= 100
            # Simulate Mediary 3.4.6 host protection on upload mount directory "130" (`待洗版/link`)!
            if "130" in del_ids:
                return self.reply({"ok": False, "error": "禁止删除受保护目录 CID 130"}, 400)
            STATE["cloud_dirs"][parent_cid] = [
                it for it in STATE["cloud_dirs"].get(parent_cid, []) if it["file_id"] not in del_ids
            ]
            STATE["recycled_115"].update(del_ids)
            return self.reply({"ok": True, "deleted": len(del_ids), "auth_source": "openapi"})

        if path == "/api/plugin/open115/files/organize":
            src_cid = body.get("source_cid", "")
            if not src_cid or src_cid == "0" or not str(src_cid).isdigit():
                return self.reply({"ok": False, "error": "source_cid 必须为有效的非根目录纯数字 CID"}, 400)
            assert body.get("task_name") == "115-895", f"Expected disabled default task_name 115-895, got {body}"
            # Real Mediary 3.4.6 inherits `target_dir="媒体库"` (`target_cid="900"`) from `task_name="115-895"`
            # ONLY when `target_cid` is omitted; passing `target_cid` rewrites root to `__cid_900__/` and breaks 3400+ folder `cloud_dirs` lookup!
            assert "target_cid" not in body, f"Must omit target_cid when task_name=115-895 owns default target_dir! Got: {body}"
            if "tmdb_id" in body:
                assert isinstance(body["tmdb_id"], int) and body["tmdb_id"] > 0, body
                assert body.get("media_type") in ("movie", "tv"), body
                if body["tmdb_id"] == 122079:
                    for f in body.get("files", []):
                        if f.get("season") == 1 and f.get("episode", 0) > 30:
                            return self.reply({"ok": False, "error": f"{f['name']} 的 S01E{f['episode']} 不在指定 TMDB 剧集季集范围内"}, 400)
            assert body.get("conflict_strategy") == "overwrite", body
            org_ids = {f["file_id"] for f in body.get("files", [])}
            assert org_ids, body
            STATE["organize_calls"].append(body)
            # Simulate Mediary 3.4.6 native single-step archive + companion subtitle follow!
            items = list(STATE["cloud_dirs"].get(src_cid, []))
            stems = [it["name"].rsplit(".", 1)[0].lower() for it in items if it["file_id"] in org_ids]
            staying = []
            moved = []
            for it in items:
                is_companion_sub = (
                    not it.get("is_dir")
                    and it["name"].lower().endswith((".ass", ".srt"))
                    and any(it["name"].lower().startswith(st) for st in stems)
                )
                if it["file_id"] in org_ids or is_companion_sub:
                    moved.append(dict(it, parent_id="200"))
                else:
                    staying.append(it)
            STATE["cloud_dirs"][src_cid] = staying
            STATE["cloud_dirs"].setdefault("200", []).extend(moved)
            if STATE.get("omit_organize_strm_paths"):
                return self.reply({"ok": True, "status": "organized", "moved": len(moved)})
            return self.reply({
                "ok": True,
                "status": "organized",
                "strm_paths": [
                    f"/strm/cloud/媒体库/电影/华语电影/2020-2030/{f['name']}.strm"
                    for f in body.get("files", [])
                ],
            })

        if path.endswith("/sources") and self.command == "DELETE":
            iid = path.split("/library/items/")[1].split("/")[0]
            STATE["deleted_sources"].setdefault(iid, set()).update(body.get("source_ids", []))
            return self.reply({"ok": True})

        if path == "/api/plugin/media-downloads":
            return self.reply({"ok": True, "id": 77})

        if path == "/api/plugin/media-downloads/77/organize":
            STATE["pt_download"]["status"] = "completed"
            return self.reply({"ok": True})

        return self.reply({"ok": True})


def run_suite(binary: str, base_url: str, data_dir: str):
    def invoke(action, payload=None, *, dry_run=True, auto_del=False, notify=False, expect_err=False):
        env = dict(
            os.environ,
            MEDIARY_PLUGIN_ACTION=action,
            MEDIARY_PLUGIN_DATA_DIR=data_dir,
            MEDIARY_PLUGIN_API_URL=base_url,
            MEDIARY_PLUGIN_TOKEN="test_token",
            MEDIARY_PLUGIN_SETTINGS_JSON=json.dumps({
                "$enabled": True,
                "dry_run": dry_run,
                "auto_delete_old": auto_del,
                "notify": notify,
            }),
        )
        proc = subprocess.run(
            [binary],
            env=env,
            input=json.dumps(payload or {}),
            text=True,
            capture_output=True,
            timeout=30,
        )
        if expect_err:
            assert proc.returncode != 0, f"Expected failure for {action}, got: {proc.stdout}"
            return proc.stderr
        assert proc.returncode == 0, f"{action} failed: {proc.stderr}"
        return json.loads(proc.stdout)

    # 0. Pre-seed a v2.0/v2.2 legacy `activity_logs` table (without `record_type`/`sub_title`) to verify smooth schema upgrade!
    db_file = Path(data_dir) / "mediwash.db"
    with sqlite3.connect(db_file) as con:
        con.execute("""
            CREATE TABLE activity_logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                kind TEXT NOT NULL,
                title TEXT NOT NULL,
                slot TEXT NOT NULL DEFAULT '',
                action TEXT NOT NULL,
                status TEXT NOT NULL,
                detail TEXT NOT NULL,
                created_at INTEGER NOT NULL
            )
        """)
        con.execute("INSERT INTO activity_logs (kind, title, slot, action, status, detail, created_at) VALUES ('115中转', '旧扫描任务', '100', 'scan_staging', 'done', '旧任务摘要', 1795600000)")

    # 1. Bootstrap & Unified Mediary Rule Sync + SQLite WAL check
    boot = invoke("ui_bootstrap")
    assert boot["host_sync"]["category_count"] == 4, boot["host_sync"]
    assert boot["host_sync"]["open115_target_cid"] == "200", boot["host_sync"]
    assert boot["host_sync"]["transfer_source_folder"] == "转存监控", boot["host_sync"]
    assert boot["host_sync"]["transfer_source_cid"] == "205", boot["host_sync"]
    assert boot["host_status"]["open_api_connected"] is True
    assert db_file.exists()
    with sqlite3.connect(db_file) as con:
        mode = con.execute("PRAGMA journal_mode").fetchone()[0]
        assert mode.lower() == "wal", f"Expected WAL mode, got {mode}"

    # 2. Dual-Sample Tester (numeric title 1917 + ISO penalty)
    cmp_res = invoke("ui_compare_samples", {
        "rule_id": "symedia:preset-v5",
        "category": "电影/华语电影/2020-2030",
        "left": {"name": "1917.2019.2160p.Blu-Ray.iso", "size_gb": 60},
        "right": {"name": "1917.2019.2160p.UHD.Blu-Ray.Remux.HEVC.TrueHD.Dolby.Atmos.7.1-CHD.mkv", "size_gb": 50},
    })
    assert cmp_res["left"]["recognition"]["parsed"]["title"] == "一九一七"
    assert cmp_res["delta"] > 70, cmp_res

    # 3. 115 Single-Directory Pipeline: Recursive Walk + Sidecar Subtitle + TMDB 404 -> `review` (NOT `reject`!)
    boot = invoke("ui_save_pipeline", {
        "pipeline": {
            "name": "MediFind 待洗版监控",
            "enabled": True,
            "media_type": "auto",
            "source_cid": "100",
            "recursive": True,
            "move_subtitles": True,
            "auto_admit": False,
            "wash_enabled": True,
            "reject_mode": "recycle_bin",
            "probe_mode": "smart",
        }
    })
    pid = boot["pipelines"][0]["id"]
    scan_out = invoke("ui_scan_pipeline", {"id": pid})
    by_fid = {it["file_id"]: it for it in scan_out["items"]}
    assert by_fid["101"]["verdict"] == "admit_upgrade", by_fid["101"]
    assert by_fid["101"]["sidecar_count"] == 1  # Paired `.chs.ass` subtitle!
    assert by_fid["103"]["verdict"] == "reject", by_fid["103"]
    assert by_fid["104"]["verdict"] == "review", "TMDB 404 must be review, NEVER reject!"
    assert by_fid["111"]["verdict"] == "admit_new", by_fid["111"]

    # 4. Dry-run protection blocks real 115 archive/delete
    err_msg = invoke("ui_staging_act", {"ids": [by_fid["101"]["id"]]}, dry_run=True, expect_err=True)
    assert "仅预览" in err_msg
    assert not STATE["writes"]

    # 5. Manual TMDB Binding turns `review` item 104 into `admit_new`!
    bound = invoke("ui_staging_bind_tmdb", {
        "id": by_fid["104"]["id"],
        "media_type": "movie",
        "tmdb_id": "88801",
        "title": "神秘新片",
        "remember": True,
    })
    by_fid = {it["file_id"]: it for it in bound["items"]}
    assert by_fid["104"]["verdict"] == "admit_new", by_fid["104"]

    # 6. Execute native 3.4.6 `files/delete` (with `parent_cid`) on 103, then `files/organize` (with integer `tmdb_id`) on 101, 104, 111!
    act1 = invoke("ui_staging_act", {"ids": [by_fid["103"]["id"]]}, dry_run=False)
    assert act1["result"]["recycled_115"] == 1
    assert "103" in STATE["recycled_115"]
    # On act1: "130" (`link` protected mount) stays intact while its empty child "131" (`电视剧/Season 01`) IS recycled!
    # Meanwhile "110" still has video "111" and "140" has `archive.zip`, so NEITHER "110" nor "140" may be deleted yet!
    assert "131" in STATE["recycled_115"] and "130" not in STATE["recycled_115"], STATE["recycled_115"]
    assert "110" not in STATE["recycled_115"] and "140" not in STATE["recycled_115"], STATE["recycled_115"]

    act2 = invoke("ui_staging_act", {"ids": [by_fid["101"]["id"], by_fid["104"]["id"], by_fid["111"]["id"]]}, dry_run=False)
    assert act2["result"]["organized"] == 3, act2
    assert act2["result"]["cleaned_dirs"] == 1, f"Mode B must auto-recycle subfolder 110 after video 111 archived despite leftover .nfo/.jpg/.txt! Got: {act2}"
    assert "110" in STATE["recycled_115"] and "140" not in STATE["recycled_115"], STATE["recycled_115"]
    archived_ids = {f["file_id"] for f in STATE["cloud_dirs"]["200"]}
    assert {"101", "102", "104", "111"} <= archived_ids, archived_ids
    # Manual TMDB binding (`88801`) MUST be passed directly to `files/organize` as integer `tmdb_id`!
    assert any(c.get("tmdb_id") == 88801 and c.get("media_type") == "movie" for c in STATE["organize_calls"]), STATE["organize_calls"]

    # 7. Post-Arrival Wash Ledger: must NOT delete old source before STRM arrives, MUST delete after arrival!
    rec_before = invoke("ui_ledger_reconcile", dry_run=False, auto_del=True)
    assert rec_before["reconcile"]["auto_cleaned"] == 0
    assert "film_alpha" not in STATE["deleted_sources"]

    STATE["library_arrived_115"] = True
    rec_after = invoke("ui_ledger_reconcile", dry_run=False, auto_del=True)
    assert rec_after["reconcile"]["auto_cleaned"] >= 1
    # Must clean low/mid superseded sources in 华语电影, while NEVER touching s_iso in 蓝光原盘!
    assert "s_iso" not in STATE["deleted_sources"].get("film_alpha", set())
    assert "s_mid" in STATE["deleted_sources"].get("film_alpha", set())

    # 8. Library Dedup Scan: zero `keep` bloat + `.iso` zone isolation!
    STATE["deleted_sources"].clear()
    STATE["library_arrived_115"] = False
    lib_res = invoke("ui_lib_control", {"scope": "movie", "operation": "start"})
    assert lib_res["job"]["mode"] == "done"
    assert lib_res["page"]["total"] == 1
    row = lib_res["page"]["items"][0]
    assert row["item_id"] == "film_alpha"
    assert row["category"] == "电影/华语电影/2020-2030"
    assert [c["id"] for c in row["candidates"]] == ["s_low_dup"]
    with sqlite3.connect(db_file) as con:
        keep_rows = con.execute("SELECT COUNT(*) FROM lib_decisions WHERE status = 'keep'").fetchone()[0]
        assert keep_rows == 0, "Single-version / keep items must never bloat SQLite!"

    clean_lib = invoke("ui_lib_act", {"ids": [row["id"]], "operation": "execute"}, dry_run=False)
    assert clean_lib["result"]["deleted_sources"] == 1
    assert STATE["deleted_sources"]["film_alpha"] == {"s_low_dup"}

    # 9. PT Active Wash: numeric `1917` -> `defer_organize=true` -> `downloaded` -> `organize` -> `clean_old`!
    lookup = invoke("ui_pt_lookup", {"keyword": "一九一七", "media_type": "movie"})
    assert lookup["items"][0]["id"] == "film_1917"
    pt_s = invoke("ui_pt_search", {"item_id": "film_1917"})
    assert len(pt_s["search"]["candidates"]) == 1, pt_s
    cand_id = pt_s["search"]["candidates"][0]["id"]
    invoke("ui_pt_act", {"id": cand_id, "operation": "download"}, dry_run=False)

    STATE["pt_download"]["status"] = "downloaded"
    # First tick triggers `/organize` -> status becomes `completed` -> verifies arrival & cleans `pt_old`!
    invoke("tick", dry_run=False, auto_del=True)
    invoke("tick", dry_run=False, auto_del=True)
    assert STATE["deleted_sources"].get("film_1917") == {"pt_old"}

    # 10. Single-Directory Auto-Archive (`files/organize`) + Direct 115 Recycle Bin (`files/delete` with `parent_cid`) + Episode Series Title Fix
    STATE["cloud_dirs"]["150"] = [
        {
            "file_id": "151",
            "parent_id": "150",
            "name": "两京十五日 (2026) - S01E05 - 2160p.WEB-DL.H265.mkv",
            "size": 1_445_007_245,  # Exact byte match with SHA1-less STRM!
            "pick_code": "pclow151",
            "sha1": "da39a3ee5e6b4b0d3255bfef95601890afd80709",
            "is_dir": False,
        },
        {
            "file_id": "152",
            "parent_id": "150",
            "name": "两京十五日 (2026) - S01E05 - 2160p.WEB-DL.H265.SMALL.mkv",
            "size": 900_000_000,
            "pick_code": "pc_low",
            "sha1": "1111111111111111111111111111111111111111",
            "is_dir": False,
        },
        {
            "file_id": "153",
            "parent_id": "150",
            "name": "热辣滚烫.2024.2160p.HDR10.WEB-DL.H265.mkv",
            "size": 9_500_000_000,
            "pick_code": "pc_new",
            "sha1": "2222222222222222222222222222222222222222",
            "is_dir": False,
        },
    ]
    boot_auto = invoke("ui_save_pipeline", {
        "pipeline": {
            "name": "单目录全自动归档",
            "enabled": True,
            "media_type": "auto",
            "source_cid": "150",
            "recursive": True,
            "move_subtitles": True,
            "auto_admit": True,
            "wash_enabled": True,
            "reject_mode": "recycle_bin",
            "probe_mode": "smart",
        },
        "run_scan": True,
    }, dry_run=False)
    assert boot_auto["scan"]["auto_moved"] == 3, boot_auto["scan"]
    archived_200 = {f["file_id"] for f in STATE["cloud_dirs"]["200"]}
    assert "153" in archived_200, archived_200
    assert {"151", "152"} <= STATE["recycled_115"], STATE["recycled_115"]
    recycled_rows = invoke("ui_staging_list", {"pipeline_id": boot_auto["pipelines"][-1]["id"], "state": "recycled_115"})["items"]
    by_150 = {r["file_id"]: r for r in recycled_rows}
    assert by_150["151"]["verdict"] == "already_in_lib"
    assert by_150["152"]["title"] == "两京十五日", f"Series title must NOT be overwritten by episode title! Got: {by_150['152']}"
    assert by_150["152"]["detail"]["episode_title"] == "假扮戏子"

    # 11. Separated Media History vs Task Logs + Filtering + Single & Filtered Delete + Clean dashboard.json
    media_hist = invoke("ui_history_list", {"record_type": "media"})
    task_hist = invoke("ui_history_list", {"record_type": "task"})
    assert media_hist["media_total"] >= 5, media_hist
    assert task_hist["task_total"] >= 2, task_hist
    assert all(it["record_type"] == "media" for it in media_hist["items"])
    assert all(it["record_type"] == "task" for it in task_hist["items"])

    kw_hist = invoke("ui_history_list", {"record_type": "media", "action_group": "recycle", "keyword": "两京十五日"})
    assert kw_hist["filtered_total"] == 2, kw_hist
    del_one = invoke("ui_history_delete", {
        "ids": [kw_hist["items"][0]["id"]],
        "record_type": "media",
        "action_group": "recycle",
        "keyword": "两京十五日",
    })
    assert del_one["deleted"] == 1
    assert del_one["filtered_total"] == 1

    # Clearing task logs must NOT delete media history!
    media_before = del_one["media_total"]
    clear_tasks = invoke("ui_history_delete", {"clear_filtered": True, "record_type": "task"})
    assert clear_tasks["task_total"] == 0
    assert clear_tasks["media_total"] == media_before

    # Test History Retry on an organized item (must re-organize in-place from archived target bucket "200" AND preserve score detail)!
    admit_hist = invoke("ui_history_list", {"record_type": "media", "action_group": "admit", "keyword": "热辣滚烫"})
    assert admit_hist["filtered_total"] >= 1, admit_hist
    assert "115://" in admit_hist["items"][0]["source_path"], admit_hist["items"][0]
    assert "/strm/cloud/媒体库/" in admit_hist["items"][0]["target_path"], admit_hist["items"][0]
    org_before_retry = len(STATE["organize_calls"])
    retry_out = invoke("ui_history_retry", {
        "ids": [admit_hist["items"][0]["id"]],
        "record_type": "media",
        "action_group": "admit",
        "keyword": "热辣滚烫",
    }, dry_run=False)
    assert retry_out["retry"]["retried"] == 1, retry_out
    assert len(STATE["organize_calls"]) == org_before_retry + 1, "Archived item retry must call files/organize from target dir!"
    assert STATE["organize_calls"][-1]["source_cid"] == "200", STATE["organize_calls"][-1]
    assert "重新整理" in retry_out["items"][0]["detail"] and "得分" in retry_out["items"][0]["detail"], retry_out["items"][0]

    # Retrying a recycled item must NEVER overwrite its original `detail`!
    rec_hist = invoke("ui_history_list", {"record_type": "media", "action_group": "recycle", "keyword": "两京十五日"})
    orig_rec_detail = rec_hist["items"][0]["detail"]
    rec_retry = invoke("ui_history_retry", {
        "ids": [rec_hist["items"][0]["id"]],
        "record_type": "media",
        "action_group": "recycle",
        "keyword": "两京十五日",
    }, dry_run=False)
    assert rec_retry["items"][0]["detail"] == orig_rec_detail, "Failed/recycled retry must NOT clobber original history detail!"

    # Diagnostic PT lookup without keyword (`source_kind=iso` -> `film_alpha`)
    iso_lookup = invoke("ui_pt_lookup", {"keyword": "", "media_type": "movie", "source_kind": "iso"})
    assert any(it["id"] == "film_alpha" for it in iso_lookup["items"]), iso_lookup

    # 12. Regression: moving an already-processed `file_id` ("153") back into a new monitoring folder ("160")
    #     with `enabled=False` MUST still be detected & archived via `files/organize` on manual `tick`!
    moved_back = [f for f in STATE["cloud_dirs"]["200"] if f["file_id"] == "153"][0]
    STATE["cloud_dirs"]["200"] = [f for f in STATE["cloud_dirs"]["200"] if f["file_id"] != "153"]
    STATE["cloud_dirs"]["160"] = [dict(moved_back, parent_id="160")]
    org_calls_before = len(STATE["organize_calls"])
    invoke("ui_save_pipeline", {
        "pipeline": {
            "name": "123测试目录",
            "enabled": False,  # Scheduled cron off, but manual tick must still scan!
            "media_type": "auto",
            "source_cid": "160",
            "recursive": True,
            "move_subtitles": True,
            "auto_admit": True,
            "wash_enabled": True,
            "reject_mode": "recycle_bin",
            "probe_mode": "smart",
        },
        "run_scan": False,
    }, dry_run=False)
    manual_tick = invoke("tick", {"manual": True}, dry_run=False)
    assert "自动执行 1 项" in manual_tick["notice"], manual_tick
    assert len(STATE["organize_calls"]) == org_calls_before + 1
    assert any(f["file_id"] == "153" for f in STATE["cloud_dirs"]["200"]), "Moved-back same file_id must be re-detected and archived!"

    # 13. Intra-batch tournament dedup: 2 candidate files for the same movie (`[tmdbid-88802]`)
    #     in `待洗版` at the same time -> higher-score winner archived, lower-score duplicate recycled!
    STATE["cloud_dirs"]["160"] = [
        {
            "file_id": "171",
            "parent_id": "160",
            "name": "多喜新片 (2026) [tmdbid-88802] - 2160p.HDR10.WEB-DL.H265.AAC.mkv",
            "size": 1_800_000_000,
            "pick_code": "pc_low",
            "sha1": "2222222222222222222222222222222222222222",
            "is_dir": False,
        },
        {
            "file_id": "172",
            "parent_id": "160",
            "name": "多喜新片 (2026) [tmdbid-88802] - 2160p.HDR10.BluRay.Remux.H265.TrueHD.Atmos.mkv",
            "size": 18_500_000_000,
            "pick_code": "pc_up",
            "sha1": "3333333333333333333333333333333333333333",
            "is_dir": False,
        },
    ]
    STATE["omit_organize_strm_paths"] = True
    dedup_tick = invoke("tick", {"manual": True}, dry_run=False, notify=True)
    STATE["omit_organize_strm_paths"] = False
    notif_calls = [b for (p, m, b) in STATE["writes"] if p == "/api/plugin/notifications"]
    assert notif_calls, "Expected templated notification on notify=True!"
    last_notif = notif_calls[-1]
    assert "✅整理完成" in last_notif["title"] and "多喜新片" in last_notif["title"], last_notif
    assert "🎞️" in last_notif["content"] and "🗑️ 拦截:" in last_notif["content"] and "🗓️ 时间:" in last_notif["content"], last_notif
    assert last_notif.get("image_url") == "https://image.tmdb.org/t/p/w780/mock_backdrop.jpg", last_notif
    assert "171" in STATE["recycled_115"], f"Lower-score intra-batch duplicate 171 must be recycled! {STATE['recycled_115']}"
    assert any(f["file_id"] == "172" for f in STATE["cloud_dirs"]["200"]), "Higher-score intra-batch winner 172 must be archived!"
    assert not any(f["file_id"] == "171" for f in STATE["cloud_dirs"]["200"]), "Lower-score 171 must NEVER collide in target dir!"

    # 14. Async library scan backfill: row 172 initially had placeholder `target_path` because
    #     Mediary's incremental scan hadn't indexed `[tmdbid-88802]` yet; once indexed with `pickcode="pc_up"`,
    #     calling `ui_history_list` must auto-backfill the real `.strm` path & category!
    pre_hist = invoke("ui_history_list", {"record_type": "media", "action_group": "admit", "keyword": "多喜新片", "status": "done"})
    assert pre_hist["filtered_total"] == 1 and "主程序二级分类归档" in pre_hist["items"][0]["target_path"], pre_hist
    STATE["extra_items"]["film_duoxi"] = {
        "id": "film_duoxi",
        "library_id": "lib1",
        "name": "多喜新片",
        "item_type": "movie",
        "tmdb_id": "88802",
        "path": "/strm/cloud/媒体库/电影/华语电影/2020-2030/多喜新片 (2026) [tmdbid-88802]",
        "sources": [{
            "id": "s_duoxi_up",
            "name": "多喜新片.2026.2160p.BluRay.REMUX.HDR10.H.265.TrueHD.strm",
            "strm_path": "/strm/cloud/媒体库/电影/华语电影/2020-2030/多喜新片 (2026) [tmdbid-88802]/多喜新片.2026.2160p.BluRay.REMUX.HDR10.H.265.TrueHD.strm",
            "file_id": "",
            "pickcode": "pc_up",
            "size": 18_500_000_000,
        }],
        "streams": [],
    }
    healed_hist = invoke("ui_history_list", {"record_type": "media", "action_group": "admit", "keyword": "多喜新片", "status": "done"})
    assert healed_hist["items"][0]["target_path"].endswith("多喜新片.2026.2160p.BluRay.REMUX.HDR10.H.265.TrueHD.strm"), healed_hist["items"][0]
    assert healed_hist["items"][0]["slot"].startswith("电影/华语电影/2020-2030 · "), healed_hist["items"][0]
    warn_hist = invoke("ui_history_list", {"record_type": "media", "keyword": "多喜新片", "status": "warning"})
    assert warn_hist["filtered_total"] == 0, warn_hist

    # 15. Cross-season absolute episode remapping (`铠甲勇士刑天 第31话` -> S02E01) + Historical false-timeout warning auto-heal!
    STATE["cloud_dirs"]["160"] = [
        {
            "file_id": "181",
            "parent_id": "160",
            "name": "铠甲勇士刑天 [tmdbid-122079] 第31话 「卷土重来」 (DVD 640x480 x264 AAC).mp4",
            "size": 320_000_000,
            "pick_code": "pc_armor31",
            "sha1": "4444444444444444444444444444444444444444",
            "is_dir": False,
        }
    ]
    with sqlite3.connect(db_file) as con:
        con.execute(
            """INSERT INTO activity_logs (kind, title, sub_title, source_path, target_path, slot, action, status, detail, created_at, record_type)
               VALUES ('115归档', '两京十五日 S01E05', '两京十五日 (2026) - S01E05 - 2160p.WEB-DL.H265.mkv',
                       '115://待洗版/两京十五日.mkv', '归档未完成（保留原目录）', '主程序自动分类 · 4K-SDR',
                       'organized', 'warning', '主程序原生归档失败：timed out reading response', 1795752216, 'media')"""
        )
    armor_tick = invoke("tick", {"manual": True}, dry_run=False)
    assert any(f["file_id"] == "181" for f in STATE["cloud_dirs"]["200"]), armor_tick
    last_org = STATE["organize_calls"][-1]
    assert last_org["tmdb_id"] == 122079 and last_org["files"][0]["season"] == 2 and last_org["files"][0]["episode"] == 1, last_org

    after_heal = invoke("ui_history_list", {"record_type": "media", "keyword": "两京十五日", "status": "warning"})
    assert after_heal["filtered_total"] == 0, f"Historical false-timeout warning must auto-heal when episode is in library! Got: {after_heal}"

    dash_json = json.loads((Path(data_dir) / "dashboard.json").read_text())
    assert all(it["record_type"] == "media" for it in dash_json["items"]), "dashboard.json must only expose clean media history!"

    sym_detail = invoke("ui_rule_detail", {"id": "symedia:preset-v5"})
    saved = invoke("ui_save_rule", {
        "id": sym_detail["id"],
        "save_as_new": True,
        "kind": "symedia",
        "name": "我的自定义规则",
        "body": sym_detail["body"],
    })
    new_rid = saved["saved_rule"]["id"]
    after_del = invoke("ui_delete_rule", {"id": new_rid})
    assert not any(r["id"] == new_rid for r in after_del["rules"])

    # 16. Mediary v3.4.8+ web_source boundary-safe extraction & title separation via `ui_compare_samples`
    cmp_ws = invoke("ui_compare_samples", {
        "rule_id": "mediatidy:ultimate-precision",
        "category": "电视剧/国产剧",
        "left": {
            "name": "藏海传.YK.2160p.WEB-DL.H265.AAC.mp4",
            "folder": "",
            "size_gb": 1.2,
            "bitrate_mbps": 8.0,
            "video_bitrate_mbps": 7.2,
        },
        "right": {
            "name": "Mad.Max.Fury.Road.2015.2160p.WEB-DL.H265.AAC-GWEB.mkv",
            "folder": "",
            "size_gb": 1.2,
            "bitrate_mbps": 8.0,
            "video_bitrate_mbps": 7.2,
        },
    })
    assert cmp_ws["left"]["profile"]["web_source"] == "YK", cmp_ws["left"]
    assert cmp_ws["left"]["recognition"]["parsed"]["title"] == "藏海传", cmp_ws["left"]
    assert cmp_ws["left"]["recognition"]["parsed"]["web_source"] == "YK", cmp_ws["left"]
    assert cmp_ws["right"]["profile"]["web_source"] == "", cmp_ws["right"]
    assert cmp_ws["right"]["recognition"]["parsed"]["title"] == "Mad Max Fury Road", cmp_ws["right"]


if __name__ == "__main__":
    binary_path = str(Path(sys.argv[1] if len(sys.argv) > 1 else "plugin").resolve())
    with ThreadingHTTPServer(("127.0.0.1", 0), MockMediary) as httpd, tempfile.TemporaryDirectory() as tmpdir:
        threading.Thread(target=httpd.serve_forever, daemon=True).start()
        run_suite(binary_path, f"http://127.0.0.1:{httpd.server_port}/api", tmpdir)
        httpd.shutdown()
    print("ALL_E2E_TESTS_PASSED: 115中转守门+外挂字幕同移+连移无错位+TMDB人工绑定+入库核验删旧+存量库零膨胀精洗+蓝光原盘隔离+PT暂缓整理洗版+双引擎编辑全通过")
