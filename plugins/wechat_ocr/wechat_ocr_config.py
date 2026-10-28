from plugin_i18n import Translator

tr = Translator(__file__, "i18n.csv")

globalOptions = {
    "title": tr("微信本地OCR"),
    "type": "group",
    "wechat_dir": {
        "title": tr("微信安装目录"),
        "default": "",
        "toolTip": tr(
            "留空则自动探测（如 /opt/wechat-universal）。"
            "也可用环境变量 WECHAT_OCR_DIR 或 ~/.config/wechat_ocr/config.json 指定。"
        ),
    },
}

localOptions = {
    "title": tr("微信本地OCR识别"),
    "type": "group",
    "timeout": {
        "title": tr("识别超时（秒）"),
        "default": 30,
        "toolTip": tr("单张图片等待微信OCR返回的最长时间"),
    },
    "qr_mode": {
        "title": tr("二维码/小程序码"),
        "optionsList": [
            ["no_text", tr("仅当图中无文字时返回")],
            ["always", tr("总是附加在文字后")],
            ["never", tr("不返回")],
        ],
        "default": "no_text",
        "toolTip": tr(
            "微信引擎可识别普通二维码（解码内容）与微信小程序码"
            "（返回原始载荷，需微信服务端解析跳转）"
        ),
    },
}
