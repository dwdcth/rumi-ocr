# -*- coding: utf-8 -*-
"""umi_log —— Umi-OCR 宿主模块复刻（规范 §4.2）。

宿主模块 plugin_i18n 依赖本模块；社区插件也可直接使用：

    from umi_log import logger
    logger.info("...")

worker 内日志输出到 stderr（规范 §3：stderr 原样转发到宿主日志）。
"""

import logging
import sys

logger = logging.getLogger("rumi-plugin")

if not logger.handlers:  # 防重复初始化，重复 import 安全
    _handler = logging.StreamHandler(sys.stderr)
    _handler.setFormatter(logging.Formatter("[rumi-plugin] %(levelname)s %(message)s"))
    logger.addHandler(_handler)
    logger.setLevel(logging.INFO)
