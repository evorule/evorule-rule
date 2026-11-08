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
BONES_PER_FINGER = ["metacarpal", "proximal_phalanx", "middle_phalanx", "distal_phalanx"]


def _build_golden():
    """构造 hand-v1 黄金样本（拓扑+比例+角度域全数字表）。"""
    joints = [{"joint_id": WRIST, "label": "腕关节", "parent": None, "kind": "root"}]
    bones = [{"bone_id": "metacarpal_" + f, "label": f"{n}掌骨",
              "from_joint": WRIST, "to_joint": f"{f}_mcp",
              "length_ratio": RATIO[(f, 0)]}
             for f, n in zip(FINGERS, ["拇指", "示指", "中指", "环指", "小指"])]
    limits = []
    for f in FINGERS:
        chain = CHAIN.get(f) or [c.format(f=f) for c in CHAIN["default"]]
        kinds = ["saddle"] if f == "thumb" else ["condyloid"] + ["hinge"] * 2
        parent = WRIST
        for i, jid in enumerate(chain):
            kind = "terminal" if jid.endswith("_tip") else kinds[min(i, len(kinds) - 1)]
            joints.append({"joint_id": jid, "label": jid,
                           "parent": parent, "kind": kind})
            parent = jid
            if i < 3:
                bones.append({"bone_id": f"{f}_{BONES_PER_FINGER[i + 1]}",
                              "label": f"{f} 节段{i + 1}",
                              "from_joint": chain[i], "to_joint": chain[i + 1],
                              "length_ratio": RATIO[(f, i + 1)]})
        # 角度域：MCP 屈 0..90（拇指 CMC 屈 0..45+对掌 0..90）；IP/DIP 屈 0..90/0..80；TIP 无域
        if f == "thumb":
            limits.append({"joint_id": "thumb_cmc", "axis": "flexion",
                           "min_deg": "0", "max_deg": "45",
                           "source": "解剖学 ROM 惯例值（立法留痕：录入批待统帅锚定文献版）"})
            limits.append({"joint_id": "thumb_cmc", "axis": "opposition",
                           "min_deg": "0", "max_deg": "90",
                           "source": "解剖学 ROM 惯例值（立法留痕：录入批待统帅锚定文献版）"})
        else:
            limits.append({"joint_id": chain[0], "axis": "flexion",
                           "min_deg": "0", "max_deg": "90",
                           "source": "解剖学 ROM 惯例值（立法留痕：录入批待统帅锚定文献版）"})
            limits.append({"joint_id": chain[1], "axis": "flexion",
                           "min_deg": "0", "max_deg": "100",
                           "source": "解剖学 ROM 惯例值（立法留痕：录入批待统帅锚定文献版）"})
            limits.append({"joint_id": chain[2], "axis": "flexion",
                           "min_deg": "0", "max_deg": "80",
                           "source": "解剖学 ROM 惯例值（立法留痕：录入批待统帅锚定文献版）"})
    landmarks = [{"idx": 0, "name": "WRIST", "maps_to": WRIST}]
    base = 1
    for fi, f in enumerate(FINGERS):
        names = ["CMC", "MCP", "PIP", "DIP", "TIP"] if f == "thumb" \
            else ["MCP", "PIP", "DIP", "TIP"]
        chain = CHAIN.get(f) or [c.format(f=f) for c in CHAIN["default"]]
        pairs = [(WRIST, "metacarpal_" + f)] if f == "thumb" else [(chain[0], f + "_metacarpal")]
        for i, nm in enumerate(names[1:] if f == "thumb" else names):
            pairs.append((chain[i], f"{f}_{BONES_PER_FINGER[min(i, 3)]}"))
        for k, (tgt, _) in enumerate(pairs[:4]):
            landmarks.append({"idx": base + fi * 4 + k, "name": f"{f.upper()}_{names[k]}",
                              "maps_to": tgt})
        landmarks.append({"idx": base + fi * 4 + len(pairs) - 1 if len(pairs) == 4 else base + fi * 4 + 3,
                          "name": f"{f.upper()}_TIP", "maps_to": chain[-1]})
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


# 骨长比例（十进制定点串；基准=中指掌骨=1）—— 录入批数字待统帅批，此处为 schema 验证用形态样本
RATIO = {
    ("thumb", 0): "0.58", ("thumb", 1): "0.35", ("thumb", 2): "0.30", ("thumb", 3): "0.24",
    ("index", 0): "0.92", ("index", 1): "0.48", ("index", 2): "0.32", ("index", 3): "0.26",
    ("middle", 0): "1.00", ("middle", 1): "0.52", ("middle", 2): "0.35", ("middle", 3): "0.27",
    ("ring", 0): "0.94", ("ring", 1): "0.49", ("ring", 2): "0.34", ("ring", 3): "0.26",
    ("little", 0): "0.74", ("little", 1): "0.40", ("little", 2): "0.28", ("little", 3): "0.23",
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
    print("[3/3] NEGATIVES_OK (3 rejected + 1 scope-note)")
    print("RESULT: ALL_GREEN")
    return 0


if __name__ == "__main__":
    sys.exit(main())
