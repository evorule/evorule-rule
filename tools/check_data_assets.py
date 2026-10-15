#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""RG-1 件A2：data/ 资产守护校验器（纯 stdlib，无新依赖）。

依据：《evorule-rule-恢复方案总设计-20261015.md》件A2 + 零删除铁律（data/ 件不删不改不移）。

三态行为：
  1. 少件→红灯 exit 1：清单 strict/sensitive 件在 data/ 缺失或改名（零删除被破坏的机器判据）；
  2. 多件→警告 exit 0（stderr）：data/ 出现清单外新文件，提醒补登记（不红灯，避免正常施工被卡）；
  3. live 件（rule.db）：存在性+只读试连（sqlite3 URI mode=ro），失败=红灯；不比对内容与尺寸。

守护语义边界（如实）：本器防「消失/改名/漂移出仓」，防不了「内容篡改后原位保留」——
内容级防篡改由各件自身机制承载（bundle BLAKE3 全包哈希、rule.db auth_audits 审计链）。

敏感三件只查存在性，本器永不读其内容。
"""
import json
import sqlite3
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DATA = ROOT / "data"
INVENTORY = DATA / "ASSET-INVENTORY.json"


def ro_try_open(db_path):
    """只读试连 SQLite：成功返回 None，失败返回错误串。绝不写入。"""
    try:
        uri = "file:" + db_path.resolve().as_posix() + "?mode=ro"
        conn = sqlite3.connect(uri, uri=True)
        try:
            conn.execute("SELECT count(*) FROM sqlite_master").fetchone()
        finally:
            conn.close()
        return None
    except Exception as exc:  # noqa: BLE001 — 单件失败原因原样上抛
        return str(exc)


def main():
    reds = []
    warns = []

    if not INVENTORY.is_file():
        print("[RED] 资产清单缺失：%s（守护前提不成立）" % INVENTORY)
        return 1
    try:
        doc = json.loads(INVENTORY.read_text(encoding="utf-8"))
        entries = doc["assets"]
        if not isinstance(entries, list) or not entries:
            raise KeyError("assets")
    except Exception as exc:  # noqa: BLE001
        print("[RED] 资产清单不可解析：%s" % exc)
        return 1

    known = set()
    n_strict = n_live = n_sens = 0
    ok_strict = ok_live = ok_sens = 0

    for ent in entries:
        path = ent.get("path")
        guard = ent.get("guard")
        if not path or guard not in ("strict", "live", "sensitive"):
            reds.append("清单条目非法（path=%r guard=%r）" % (path, guard))
            continue
        known.add(path)
        f = DATA.joinpath(*path.split("/"))

        if guard == "strict":
            n_strict += 1
            if f.is_file():
                ok_strict += 1
            else:
                reds.append("strict 件缺失/改名：%s" % path)
        elif guard == "sensitive":
            n_sens += 1
            if f.exists():
                ok_sens += 1
            else:
                reds.append("sensitive 件缺失：%s" % path)
        else:  # live
            n_live += 1
            if not f.is_file():
                reds.append("live 件缺失：%s" % path)
            else:
                err = ro_try_open(f)
                if err is None:
                    ok_live += 1
                else:
                    reds.append("live 件只读试连失败：%s（%s）" % (path, err))

    # 清单外新文件（含子目录）
    import os

    actual = set()
    for root, dirs, files in os.walk(DATA):
        dirs[:] = [d for d in dirs if d != ".git"]
        for name in files:
            rel = Path(root, name).relative_to(DATA).as_posix()
            if rel != INVENTORY.name:
                actual.add(rel)
    for extra in sorted(actual - known):
        warns.append("清单外新文件（请补登记 ASSET-INVENTORY.json）：%s" % extra)

    for w in warns:
        print("[WARN] %s" % w, file=sys.stderr)
    for r in reds:
        print("[RED] %s" % r)

    print("check_data_assets：strict %d/%d | live %d/%d（只读试连） | sensitive %d/%d | 清单外新件 %d"
          % (ok_strict, n_strict, ok_live, n_live, ok_sens, n_sens, len(warns)))
    if reds:
        print("结果：红灯（exit 1），红灯数=%d" % len(reds))
        return 1
    print("结果：全绿（exit 0）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
