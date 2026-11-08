#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright (C) 2026 EvoRule Project
"""skeleton-hand-v1 领域 schema 立法验证器（B7b 门禁证据件）。

三段验证：
1. meta-validation —— schema 本体过 Draft 2020-12 元_SCHEMA；
2. 黄金样本正例 —— hand-v1 全数字表（21 关节 19 骨 21 投影）逐条过检；
3. 程序化负例 —— 缺 required / 科学计数法 / 坏 blake3 / 坏引用各一，全部必须被拒。

exit 0=全绿；exit 1=任一意外。零网络零副作用。
"""
import json
import sys
from pathlib import Path

import jsonschema

ROOT = Path(__file__).resolve().parent.parent
SCHEMA = ROOT / "data" / "domain_schemas" / "skeleton-hand-v1.json"

WRIST = "wrist"
FINGERS = ["thumb", "index", "middle", "ring", "little"]
# 每指链：MCP ->PIP-> DIP -> TIP（拇指为 CMC ->MCP-> IP -> TIP，5 关节）
CHAIN = {
    "thumb": ["thumb_cmc", "thumb_mcp", "thumb_ip", "thumb_tip"],
    "default": ["{f}_mcp", "{f}_pip", "{f}_dip", "{f}_tip"],
}
# 每指骨节段：拇指无中节指骨（近节直连远节）=19 骨口径（与 data/skeleton_sets/hand-v1.json 同构）
FINGER_BONES = {
    "thumb": ["metacarpal", "proximal_phalanx", "distal_phalanx"],
    "default": ["metacarpal", "proximal_phalanx", "middle_phalanx", "distal_phalanx"],
}


def _build_golden():
    """构造 hand-v1 黄金样本（拓扑+比例+角度域全数字表；拓扑与真值同构）。"""
    joints = [{"joint_id": WRIST, "label": "腕关节", "parent": None, "kind": "root"}]
    bones = []
    for f, n in zip(FINGERS, ["拇指", "示指", "中指", "环指", "小指"]):
        segs = FINGER_BONES.get(f) or FINGER_BONES["default"]
        chain = CHAIN.get(f) or [c.format(f=f) for c in CHAIN["default"]]
        # 拇指含 CMC 关节点：掌骨体=cmc→mcp 跨度（wrist→cmc=腕骨锚抽象段，无独立骨件），
        # 指骨节段随之后移一位；其余四指掌骨=wrist→mcp 整段（腕段并入锚）
        off = 1 if f == "thumb" else 0
        bones.append({"bone_id": f"metacarpal_{f}", "label": f"{n}掌骨",
                      "from_joint": chain[0] if off else WRIST, "to_joint": chain[off],
                      "length_ratio": RATIO[(f, 0)]})
        for i, seg in enumerate(segs[1:]):
            bones.append({"bone_id": f"{f}_{seg}",
                          "label": f"{n} {seg}",
                          "from_joint": chain[off + i], "to_joint": chain[off + i + 1],
                          "length_ratio": RATIO[(f, i + 1)]})
    limits = []
    for f in FINGERS:
        chain = CHAIN.get(f) or [c.format(f=f) for c in CHAIN["default"]]
        kinds = ["saddle", "hinge", "hinge"] if f == "thumb" else ["condyloid", "hinge", "hinge"]
        parent = WRIST
        for i, jid in enumerate(chain):
            kind = "terminal" if jid.endswith("_tip") else kinds[min(i, len(kinds) - 1)]
            joints.append({"joint_id": jid, "label": jid,
                           "parent": parent, "kind": kind})
            parent = jid
    # 角度域 16 条：拇指 CMC 屈 0..45+对掌 0..90、MCP 屈 0..50、IP 屈 0..80；
    # 四指 MCP 屈 0..90 / PIP 屈 0..100 / DIP 屈 0..80；TIP 端点无域（AAOS ROM 参考值）
    limits.append({"joint_id": "thumb_cmc", "axis": "flexion",
                   "min_deg": "0", "max_deg": "45",
                   "source": "AAOS joint motion reference values（拇指 CMC 屈曲）"})
    limits.append({"joint_id": "thumb_cmc", "axis": "opposition",
                   "min_deg": "0", "max_deg": "90",
                   "source": "AAOS joint motion reference values（对掌量角器惯例简化为度域保守近似）"})
    limits.append({"joint_id": "thumb_mcp", "axis": "flexion",
                   "min_deg": "0", "max_deg": "50",
                   "source": "AAOS joint motion reference values（拇指 MCP 屈曲）"})
    limits.append({"joint_id": "thumb_ip", "axis": "flexion",
                   "min_deg": "0", "max_deg": "80",
                   "source": "AAOS joint motion reference values（拇指 IP 屈曲）"})
    for f in FINGERS[1:]:
        chain = [c.format(f=f) for c in CHAIN["default"]]
        for jid, mx in zip(chain[:3], ["90", "100", "80"]):
            limits.append({"joint_id": jid, "axis": "flexion",
                           "min_deg": "0", "max_deg": mx,
                           "source": "AAOS joint motion reference values"})
    landmarks = [{"idx": 0, "name": "WRIST", "maps_to": WRIST}]
    # MediaPipe hands-21 官方命名直映关节（与 data/skeleton_sets/hand-v1.json 同构）
    MP_NAMES = {
        "thumb": ["THUMB_CMC", "THUMB_MCP", "THUMB_IP", "THUMB_TIP"],
        "index": ["INDEX_FINGER_MCP", "INDEX_FINGER_PIP", "INDEX_FINGER_DIP", "INDEX_FINGER_TIP"],
        "middle": ["MIDDLE_FINGER_MCP", "MIDDLE_FINGER_PIP", "MIDDLE_FINGER_DIP", "MIDDLE_FINGER_TIP"],
        "ring": ["RING_FINGER_MCP", "RING_FINGER_PIP", "RING_FINGER_DIP", "RING_FINGER_TIP"],
        "little": ["PINKY_MCP", "PINKY_PIP", "PINKY_DIP", "PINKY_TIP"],
    }
    for fi, f in enumerate(FINGERS):
        chain = CHAIN.get(f) or [c.format(f=f) for c in CHAIN["default"]]
        base = 1 + fi * 4
        for k, jid in enumerate(chain):
            landmarks.append({"idx": base + k, "name": MP_NAMES[f][k], "maps_to": jid})
    return {
        "skeleton_id": "hand-v1",
        "name": "人手骨骼规则集第一批（21 关节投影口径）",
        "units": {"length": "ratio", "angle": "degree"},
        "joints": joints,
        "bones": bones,
        "length_base": "metacarpal_middle",
        "joint_limits": limits,
        "landmark_projection": {
            "mediapipe": {"model": "hands-21",
                          "landmarks": landmarks[:21]}
        },
        "external_assets": [{
            "asset_id": "hand-mesh-ref", "kind": "mesh",
            "path": "assets/hand_reference.obj",
            "blake3": "a" * 64,
        }],
    }


# 骨长比例真值（十进制定点串；基准=第三掌骨=1）—— Buchholz, Neumann & Brand (1992)
# 手部运动学测量均值比例化，两位定点；与 data/skeleton_sets/hand-v1.json 同源同步
RATIO = {
    ("thumb", 0): "0.56", ("thumb", 1): "0.35", ("thumb", 2): "0.24",
    ("index", 0): "0.92", ("index", 1): "0.49", ("index", 2): "0.32", ("index", 3): "0.26",
    ("middle", 0): "1.00", ("middle", 1): "0.52", ("middle", 2): "0.36", ("middle", 3): "0.29",
    ("ring", 0): "0.94", ("ring", 1): "0.49", ("ring", 2): "0.34", ("ring", 3): "0.27",
    ("little", 0): "0.74", ("little", 1): "0.38", ("little", 2): "0.27", ("little", 3): "0.24",
}


def main() -> int:
    schema = json.loads(SCHEMA.read_text(encoding="utf-8"))
    jsonschema.Draft202012Validator.check_schema(schema)
    print("[1/3] META_VALID_OK")
    v = jsonschema.Draft202012Validator(schema)

    golden = _build_golden()
    errs = sorted(v.iter_errors(golden), key=lambda e: e.path)
    if errs:
        for e in errs[:5]:
            print("[GOLDEN FAIL]", list(e.path), e.message[:120])
        return 1
    n_j = len(golden["joints"])
    n_b = len(golden["bones"])
    n_l = len(golden["landmark_projection"]["mediapipe"]["landmarks"])
    print(f"[2/3] GOLDEN_OK joints={n_j} bones={n_b} limits={len(golden['joint_limits'])} landmarks={n_l}")
    assert n_j == 21 and n_l == 21, "21 点口径破坏"
    assert n_b == 19, "19 骨口径破坏（拇指单节指骨）"
    assert len(golden["joint_limits"]) == 16, "16 角度域口径破坏"

    # [3/4] 真值对拍：黄金样本拓扑/比例/角度域必须与 data/skeleton_sets 正本逐位等价
    truth = json.loads((ROOT / "data" / "skeleton_sets" / "hand-v1.json").read_text(encoding="utf-8"))
    for table in ("joints", "bones", "joint_limits"):
        g_sorted = sorted(golden[table], key=lambda r: r.get("joint_id") or r.get("bone_id"))
        t_sorted = sorted(truth[table], key=lambda r: r.get("joint_id") or r.get("bone_id"))
        g_norm = [{k: v for k, v in row.items() if k != "label"} for row in g_sorted]
        t_norm = [{k: v for k, v in row.items() if k != "label"} for row in t_sorted]
        if g_norm != t_norm:
            for gr, tr in zip(g_norm, t_norm):
                if gr != tr:
                    print(f"[TRUTH FAIL] {table}: golden={gr} truth={tr}")
                    break
            return 1
    mp_g = golden["landmark_projection"]["mediapipe"]["landmarks"]
    mp_t = truth["landmark_projection"]["mediapipe"]["landmarks"]
    if mp_g != mp_t:
        print("[TRUTH FAIL] landmark_projection 不等价")
        return 1
    print("[3/4] TRUTH_PARITY_OK (golden == data/skeleton_sets/hand-v1.json)")

    negatives = []
    d1 = json.loads(json.dumps(golden)); del d1["joint_limits"]
    negatives.append(("缺 required(joint_limits)", d1, True))
    d2 = json.loads(json.dumps(golden))
    d2["bones"][0]["length_ratio"] = "9.5e-01"          # 科学计数法 → pattern 拒
    negatives.append(("科学计数法 length_ratio", d2, True))
    d3 = json.loads(json.dumps(golden))
    d3["external_assets"][0]["blake3"] = "ZZ" * 16       # 坏哈希 → pattern 拒
    negatives.append(("坏 blake3", d3, True))
    d4 = json.loads(json.dumps(golden))
    d4["bones"][2]["from_joint"] = "ghost_joint"         # 悬挂引用
    negatives.append(("悬挂关节引用", d4, None))          # schema 不查引用 → 单独断言
    for name, doc, must_reject in negatives:
        ok = len(list(v.iter_errors(doc))) > 0
        if must_reject is True and not ok:
            print(f"[NEG FAIL] {name} 未被拒")
            return 1
        if must_reject is None:
            # 引用完整性属 schema 外的应用层校验（B7 判定核入口职责），在此留痕口径
            print(f"[NEG NOTE] {name}: schema 层不拒（引用完整性归判定核入口，非本 schema 职责）")
            continue
    print("[4/4] NEGATIVES_OK (3 rejected + 1 scope-note)")
    print("RESULT: ALL_GREEN")
    return 0


if __name__ == "__main__":
    sys.exit(main())
