# -*- coding: utf-8 -*-
"""凭据统一读取器（凭据解耦批，统帅裁S4 立项 2026-10-15）。

纪律：仓内新脚本一律走本读取器，禁止硬编码凭据绝对路径（data/ 存量 35 件
历史验收脚本冻结留痕，不回改）；token 存档通道已淘汰（裁S3）——token 一律
现登现取，不落盘（data/token.txt 已降格为过期 JWT 快照证据件，不作凭据用）。

用法：
    from credentials import admin_credentials, bearer_token
    u, p = admin_credentials()
    tok = bearer_token()          # 缺省 http://127.0.0.1:8081
    tok = bearer_token("http://127.0.0.1:18081")

环境变量：
    EVORULE_RULE_URL          rule serve 地址（缺省 http://127.0.0.1:8081）
    EVORULE_ADMIN_CRED_FILE   admin 凭据存档路径（缺省 data/admin-credentials.local.txt）

本文件不含凭据值，入 git 受版本控制。
"""
import json
import os
import pathlib
import urllib.request

_RULE_DEFAULT = "http://127.0.0.1:8081"
_DATA = pathlib.Path(__file__).resolve().parent.parent / "data"


def admin_credentials():
    """读取 admin 凭据，返回 (username, password)。

    优先环境变量 EVORULE_ADMIN_CRED_FILE，兜底 data/admin-credentials.local.txt
    （键值行格式：username=… / password=…）。
    """
    path = pathlib.Path(
        os.environ.get("EVORULE_ADMIN_CRED_FILE", str(_DATA / "admin-credentials.local.txt"))
    )
    kv = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        if "=" in line:
            k, v = line.split("=", 1)
            kv[k.strip()] = v.strip()
    return kv["username"], kv["password"]


def bearer_token(rule_url=None):
    """现登现取 access_token（不落盘），返回 str。

    用 admin_credentials 登录 rule serve（POST /v1/auth/login，tenant=default）
    换取新 token；地址优先级：入参 > EVORULE_RULE_URL > 缺省 8081。
    """
    url = (rule_url or os.environ.get("EVORULE_RULE_URL") or _RULE_DEFAULT).rstrip("/")
    u, p = admin_credentials()
    req = urllib.request.Request(
        url + "/v1/auth/login",
        data=json.dumps({"tenant_id": "default", "username": u, "password": p}).encode(),
        headers={"Content-Type": "application/json"},
    )
    return json.loads(urllib.request.urlopen(req, timeout=10).read())["access_token"]
