# -*- coding: utf-8 -*-
"""umi_worker.py —— Rumi 插件 worker 入口（Umi-OCR 插件协议兼容层）。

用法：<python> umi_worker.py <plugin_dir> [--lang zh_CN]

stdio 按行 JSON 协议（规范 §3，每行一个 JSON 对象）：
  host→worker  {"id":1,"method":"info"}
               {"id":2,"method":"init","global":{...}}      # 实例化 Api(globalArgd)
               {"id":3,"method":"start","local":{...}}      # 回 {"ok":bool,"error":str}
               {"id":4,"method":"runPath","path":"..."}
               {"id":5,"method":"runBytes","b64":"..."}     # bytes 经 base64 过 JSON
               {"id":6,"method":"runBase64","b64":"..."}
               {"id":7,"method":"stop"}
  worker→host  {"id":..,"ok":true,"result":..}
               {"id":..,"ok":false,"error":<traceback>}     # 未捕获异常

- 插件以「目录名 = 包名」方式 import：plugin_dir 的父目录注入 sys.path；
  宿主模块（plugin_i18n / umi_log）由本文件所在 pyshim 目录提供。
- stdout 只走 JSON 协议；插件 print / 引擎日志改道 stderr（规范 §3）。
- 插件导入失败不退出 worker：错误逐次经协议回报宿主（规范 §4 尾注），
  宿主可在插件详情里显示"缺少宿主模块 xxx"。
"""

import base64
import json
import os
import sys
import traceback

# 协议输出流：必须在把 sys.stdout 重定向到 stderr 之前保存原对象
_PROTO_OUT = None
_LANG = ""
_PACKAGE = ""
_STATE = {"done": False, "error": None, "info": None, "plugin": None, "api": None}


class PluginLoadError(Exception):
    """插件（或其依赖的宿主模块）导入失败。"""


def _parse_args(argv):
    plugin_dir = None
    lang = ""
    i = 0
    while i < len(argv):
        a = argv[i]
        if a == "--lang" and i + 1 < len(argv):
            i += 1
            lang = argv[i]
        elif a.startswith("--lang="):
            lang = a.split("=", 1)[1]
        elif not a.startswith("-"):
            plugin_dir = a
        i += 1
    return plugin_dir, lang


def _to_jsonable(v):
    """把配置 schema 递归转成纯 JSON 可表达的结构（规范 §3：schema 已转纯 JSON）。"""
    if v is None or isinstance(v, (bool, int, float, str)):
        return v
    if isinstance(v, dict):
        return {str(k): _to_jsonable(x) for k, x in v.items()}
    if isinstance(v, (list, tuple)):
        return [_to_jsonable(x) for x in v]
    return str(v)  # 其余类型（如误放进 schema 的对象）降级为字符串


def _load_plugin():
    """懒加载插件。import 失败不退出 worker，之后每次调用重复回报同一错误。"""
    if _STATE["done"]:
        if _STATE["error"]:
            raise PluginLoadError(_STATE["error"])
        return

    import plugin_i18n  # 宿主模块（pyshim 目录在 sys.path 上）

    if _LANG:
        # 必须先于插件 import：配置 schema 在 import 时求值 tr()（规范 §2）
        plugin_i18n.setLangCode(_LANG)
    try:
        import importlib

        mod = importlib.import_module(_PACKAGE)
        info = mod.PluginInfo
        _STATE["info"] = {
            "name": _PACKAGE,
            "group": _to_jsonable(info.get("group", "")),
            "global_options": _to_jsonable(info.get("global_options")),
            "local_options": _to_jsonable(info.get("local_options")),
        }
        _STATE["plugin"] = info
        _STATE["error"] = None
    except Exception:
        _STATE["error"] = traceback.format_exc()
        raise PluginLoadError(_STATE["error"])
    finally:
        _STATE["done"] = True


def _handle(req):
    method = req.get("method")
    _load_plugin()

    if method == "info":
        return _STATE["info"]

    if method == "init":
        api = _STATE["plugin"]["api_class"](req.get("global") or {})  # Api(globalArgd)
        _STATE["api"] = api
        return None

    api = _STATE["api"]
    if method == "start":
        ret = api.start(req.get("local") or {})
        ok = ret == ""
        return {"ok": bool(ok), "error": "" if ok else str(ret)}
    if method == "runPath":
        return api.runPath(req.get("path", ""))
    if method == "runBytes":
        return api.runBytes(base64.b64decode(req.get("b64", "")))
    if method == "runBase64":
        return api.runBase64(req.get("b64", ""))
    if method == "stop":
        if api is not None:
            api.stop()
        return None

    raise KeyError(f"未知方法：{method!r}")


def _send(obj):
    _PROTO_OUT.write(json.dumps(obj, ensure_ascii=False) + "\n")
    _PROTO_OUT.flush()


def main(argv):
    global _LANG, _PACKAGE, _PROTO_OUT

    plugin_dir, _LANG = _parse_args(argv)
    if not plugin_dir:
        print("用法：umi_worker.py <plugin_dir> [--lang zh_CN]", file=sys.stderr)
        return 2

    plugin_dir = os.path.normpath(os.path.abspath(plugin_dir))
    _PACKAGE = os.path.basename(plugin_dir)
    if not _PACKAGE:
        print(f"非法插件目录：{plugin_dir}", file=sys.stderr)
        return 2

    pyshim_dir = os.path.dirname(os.path.abspath(__file__))
    parent_dir = os.path.dirname(plugin_dir)
    # 依次插到最前 → 最终顺序：pyshim（宿主模块优先）在前，插件父目录其后
    for p in (parent_dir, pyshim_dir):
        if p and p not in sys.path:
            sys.path.insert(0, p)

    # stderr 固定 UTF-8：插件 print / 引擎日志不随 locale 抖动
    try:
        sys.stderr.reconfigure(encoding="utf-8")
    except Exception:
        pass

    # stdout 只走 JSON 协议；插件 print 改道 stderr（规范 §3：插件 print 走 stderr）
    _PROTO_OUT = sys.stdout
    try:
        _PROTO_OUT.reconfigure(encoding="utf-8")
    except Exception:
        pass
    sys.stdout = sys.stderr

    while True:
        line = sys.stdin.readline()
        if not line:  # 宿主关闭 stdin / 宿主退出 → worker 退出
            break
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
            if not isinstance(req, dict):
                raise ValueError("请求必须是 JSON 对象")
        except Exception as e:
            _send({"id": None, "ok": False, "error": f"请求解析失败：{e}"})
            continue
        try:
            result = _handle(req)
            _send({"id": req.get("id"), "ok": True, "result": result})
        except Exception:
            # 未捕获异常按 {"id":..,"ok":false,"error":traceback} 回报（规范 §3）
            _send({"id": req.get("id"), "ok": False, "error": traceback.format_exc()})
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
