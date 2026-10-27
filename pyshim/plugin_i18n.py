# -*- coding: utf-8 -*-
"""plugin_i18n —— Umi-OCR 宿主模块复刻（规范 §4.1，语义照抄 Umi 源码）。

插件用法（import 时即求值，故 worker 须先 setLangCode 再 import 插件）：

    from plugin_i18n import Translator
    tr = Translator(__file__, "i18n.csv")
    title = tr("阿巴阿巴OCR")

CSV 格式：首行 header 为语言码列，第 0 列固定为原文。例（demo_AbaOCR/i18n.csv）：

    key,en_US
    阿巴阿巴OCR,Aba OCR

列查找/回退规则：找 LangCode 列；找不到时 zh_* → 第 0 列（原文），否则第 1 列。
"""

import csv
import os

from umi_log import logger

# 当前语言码，由 worker 启动时 setLangCode 设置（必须先于插件 import）
LangCode = ""


def setLangCode(langCode):
    global LangCode
    LangCode = langCode


class Translator:
    def __init__(self, file="", name=""):
        # 读 os.path.dirname(file)/name 的 CSV；header 是语言码列
        self._trDict = {}
        path = os.path.join(os.path.dirname(file), name) if name else file
        try:
            # utf-8-sig：兼容带 BOM 的 CSV；newline="" 是 csv 模块要求
            with open(path, "r", encoding="utf-8-sig", newline="") as f:
                reader = csv.reader(f)
                headers = next(reader, None)
                if not headers:
                    return
                if LangCode in headers:
                    index = headers.index(LangCode)
                elif LangCode.startswith("zh"):
                    index = 0
                else:
                    index = 1
                for row in reader:
                    if len(row) > index:
                        self._trDict[row[0]] = row[index]
        except OSError as e:
            logger.error("Translator 读取翻译表失败：%s（%s）", path, e)

    def __call__(self, original):
        return self._trDict.get(original, original)
