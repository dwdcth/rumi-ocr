from . import wechat_ocr_config
from . import wechat_ocr_api

# 插件信息
PluginInfo = {
    # 插件组别
    "group": "ocr",
    # 全局配置
    "global_options": wechat_ocr_config.globalOptions,
    # 局部配置
    "local_options": wechat_ocr_config.localOptions,
    # 接口类
    "api_class": wechat_ocr_api.Api,
}
