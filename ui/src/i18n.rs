//! i18n —— UI 词条表（P2 #15）：zh_CN 为基准，en_US 缺词回退 zh_CN。
//!
//! 词条存于 `ui/assets/i18n/{zh-CN,en-US}.json`（编译期 include_str! 嵌入，
//! serde 零拷贝查表）——不写死在代码里；`k` 常量区只保留 key 标识符。
//!
//! 结构：[`t(lang, key)`] 纯函数查表。en 词表用 `Option` 匹配，**任何语言**
//! 都先落到 zh 表兜底（en 没有的词、以及未知语言 zh_TW/ja_JP，直接显示 zh），
//! 未知 key 返回 key 本身（方便发现漏配）。
//!
//! 即时生效：App 持有 `lang` Signal（初始值 settings.lang），保存设置时 set；
//! 各页面组件在渲染体里 `lang()` 一次建立订阅，语言切换后整页重渲染、
//! `t()` 按 settings.lang 重新取词。settings.lang 同时仍是插件 worker --lang 的来源。
//!
//! 非组件上下文（spawn 闭包 / std 线程回调里产生的状态消息）拿不到 lang 参数：
//! 语言另存全局原子（[`set_lang`] / [`lang`]，AtomicU8），[`tt`] 读原子取词。
//! main() 启动时按 settings.lang 初始化，设置页保存时同步写——组件路径
//! （ts）与线程路径（tt）看到的是同一份语言。
//!
//! 测试：`cargo test -p rumi-ui i18n`（en 缺词回退 / 未知语言回退 / 未知 key /
//! 全局原子切换 / 托盘与状态词条双语齐全）。

/// 取词：`lang` 形如 "zh_CN" / "en_US" / "zh_TW" / "ja_JP"。
/// en* 走英文表（缺词回退 zh）；其余语言一律中文。
/// 未知 key 原样返回（方便发现漏配），故返回值生命周期跟随 key。
pub fn t<'a>(lang: &str, key: &'a str) -> &'a str {
    if lang.starts_with("en") {
        if let Some(s) = en(key) {
            return s;
        }
    }
    zh(key)
}

/// 组件内取词的便捷入口：key 恒为 [`k`] 里的 &'static 常量，
/// 返回 &'static str，闭包 `let t = |k: &'static str| i18n::ts(&lang, k)`
/// 不受生命周期推断拖累（rsx 组件体里大量使用的就是它）。
pub fn ts(lang: &str, key: &'static str) -> &'static str {
    t(lang, key)
}

// ── 全局语言原子：非组件上下文（spawn 闭包 / std 线程）取词用 ────────────────
//
// 状态消息在 ocr_flow / batch / main 的 spawn 闭包里产生，拿不到组件的 lang
// Signal；把语言做成进程级 AtomicU8（0=zh 基准，1=en），main() 按
// settings.lang 初始化、设置页保存时同步写。`tt()` 读原子取词，组件内的
// ts() 仍按渲染时快照取词——两处由同一份设置驱动，不会分叉。

use std::sync::atomic::{AtomicU8, Ordering};

static LANG: AtomicU8 = AtomicU8::new(0);
const LANG_ZH: u8 = 0;
const LANG_EN: u8 = 1;

/// 设置全局语言（en* → 英文，其余一律中文基准）。main() / 设置页保存时调用。
pub fn set_lang(lang: &str) {
    LANG.store(
        if lang.starts_with("en") { LANG_EN } else { LANG_ZH },
        Ordering::Relaxed,
    );
}

/// 当前全局语言（规范串 "zh_CN" / "en_US"；history_age 等非组件调用点可用）
pub fn lang() -> &'static str {
    match LANG.load(Ordering::Relaxed) {
        LANG_EN => "en_US",
        _ => "zh_CN",
    }
}

/// 非组件上下文取词：读全局原子语言（spawn 闭包里直接 `i18n::tt(key)`）。
/// 组件内仍用 [`ts`]（渲染快照 + 信号订阅）。
pub fn tt(key: &'static str) -> &'static str {
    t(lang(), key)
}

/// 词条 key 常量（rsx 里引用，避免手写字符串打错）
pub mod k {
    // 导航 / 页题
    pub const NAV_RECOGNIZE: &str = "nav.recognize";
    pub const NAV_BATCH: &str = "nav.batch";
    pub const NAV_PLUGINS: &str = "nav.plugins";
    pub const NAV_SETTINGS: &str = "nav.settings";
    pub const PAGE_RECOGNIZE: &str = "page.recognize";
    pub const PAGE_BATCH: &str = "page.batch";
    pub const PAGE_SETTINGS: &str = "page.settings";

    // 识别页按钮 / 状态
    pub const BTN_FULLSCREEN: &str = "btn.fullscreen";
    pub const BTN_REGION: &str = "btn.region";
    pub const BTN_WINDOW: &str = "btn.window";
    pub const BTN_PASTE: &str = "btn.paste";
    pub const BTN_RECOGNIZING: &str = "btn.recognizing";
    pub const BTN_COPY_ALL: &str = "btn.copy_all";
    pub const BTN_COPIED: &str = "btn.copied";
    pub const BTN_SWITCH_ENGINE: &str = "btn.switch_engine";

    // 识别页结果 / 空态
    pub const MAIN_RESULTS_HEAD: &str = "main.results_head";
    pub const MAIN_NO_TEXT: &str = "main.no_text";
    pub const MAIN_EMPTY_TITLE: &str = "main.empty_title";
    pub const MAIN_EMPTY_SUB: &str = "main.empty_sub";
    pub const MAIN_EDITABLE_HINT: &str = "main.editable_hint";
    pub const MAIN_ENGINE: &str = "main.engine";

    // 排版解析（P2 #11）
    pub const LAYOUT_LABEL: &str = "layout.label";
    pub const LAYOUT_BY_LINE: &str = "layout.by_line";
    pub const LAYOUT_MERGE: &str = "layout.merge_para";
    pub const LAYOUT_NATURAL: &str = "layout.natural_para";

    // 公式模式（J2：识别页工具行开关）
    pub const FORMULA_TOGGLE: &str = "formula.toggle";
    pub const FORMULA_HINT: &str = "formula.hint";
    /// 公式引擎冷启动首跑状态行提示（首次下载 ~230MB 模型）
    pub const STA_FORMULA_INIT: &str = "sta.formula_init";
    /// O 波 #6：启动后台预热提示（formula_mode 开时与文字引擎并列）
    pub const STA_FORMULA_WARMING: &str = "sta.formula_warming";
    /// O 波 #6：后台预热完成提示
    pub const STA_FORMULA_WARMED: &str = "sta.formula_warmed";

    // 历史记录（P1 #7）/ 划选（P1 #9）
    pub const HIST_TITLE: &str = "hist.title";
    pub const HIST_CLEAR: &str = "hist.clear";
    pub const HIST_EMPTY: &str = "hist.empty";
    pub const HIST_RESTORE_HINT: &str = "hist.restore_hint";
    pub const SEL_COPIED: &str = "sel.copied";
    pub const SEL_HINT: &str = "sel.hint";

    // 历史条目相对时间
    pub const AGE_JUST_NOW: &str = "age.just_now";
    pub const AGE_MIN: &str = "age.min";
    pub const AGE_HOUR: &str = "age.hour";
    pub const AGE_DAY: &str = "age.day";

    // 设置页
    pub const SET_HOTKEY: &str = "set.hotkey";
    pub const SET_PORT: &str = "set.port";
    pub const SET_LANG: &str = "set.lang";
    pub const SET_THEME: &str = "set.theme";
    pub const SET_THEME_DARK: &str = "set.theme_dark";
    pub const SET_THEME_LIGHT: &str = "set.theme_light";
    /// Wave3：无边框窗口（自定义标题栏）开关
    pub const SET_FRAMELESS: &str = "set.frameless";
    pub const SET_FRAMELESS_HINT: &str = "set.frameless_hint";
    /// M 波：选区方式三选（跟随后端 / 原生 / 内置）
    pub const SET_PICKER: &str = "set.picker";
    pub const SET_PICKER_HINT: &str = "set.picker_hint";
    pub const SET_PICKER_AUTO: &str = "set.picker_auto";
    pub const SET_PICKER_NATIVE: &str = "set.picker_native";
    pub const SET_PICKER_BUILTIN: &str = "set.picker_builtin";
    /// Wave3：自定义标题栏按钮 tooltip
    pub const TB_MINIMIZE: &str = "tb.minimize";
    pub const TB_HIDE: &str = "tb.hide";
    pub const SET_ENGINE: &str = "set.engine";
    pub const SET_MANAGE_ENGINE: &str = "set.manage_engine";
    pub const SET_NOTIFY: &str = "set.notify";
    pub const SET_NOTIFY_HINT: &str = "set.notify_hint";
    pub const SET_SOUND: &str = "set.sound";
    pub const SET_SOUND_HINT: &str = "set.sound_hint";
    pub const SET_IGNORE: &str = "set.ignore";
    pub const SET_IGNORE_HINT: &str = "set.ignore_hint";
    pub const SET_SAVE: &str = "set.save";
    pub const SET_EFFECT: &str = "set.effect";

    // 批量页
    pub const BATCH_START: &str = "batch.start";
    pub const BATCH_PICK: &str = "batch.pick";
    pub const BATCH_EXPORT_TXT: &str = "batch.export_txt";
    pub const BATCH_EXPORT_MD: &str = "batch.export_md";
    pub const BATCH_REMOVE: &str = "batch.remove";
    pub const BATCH_WITH_QR: &str = "batch.with_qr";
    pub const BATCH_EMPTY_TITLE: &str = "batch.empty_title";
    pub const BATCH_EMPTY_SUB: &str = "batch.empty_sub";
    pub const BATCH_QR_TITLE: &str = "batch.qr_title";
    pub const BATCH_QR_TEXT: &str = "batch.qr_text";
    pub const BATCH_QR_SCALE: &str = "batch.qr_scale";
    pub const BATCH_QR_GENERATE: &str = "batch.qr_generate";
    pub const BATCH_QR_SAVE: &str = "batch.qr_save";
    pub const BATCH_QR_PLACEHOLDER: &str = "batch.qr_placeholder";

    // 状态词
    pub const STATUS_READY: &str = "status.ready";

    // ── 托盘菜单（tray.rs build_menu；语言切换时整菜单重建）──
    pub const TRAY_SHOW: &str = "tray.show";
    pub const TRAY_OCR: &str = "tray.ocr";
    pub const TRAY_OCR_PAUSED: &str = "tray.ocr_paused";
    pub const TRAY_BATCH: &str = "tray.batch";
    pub const TRAY_PAUSE_HOTKEY: &str = "tray.pause_hotkey";
    pub const TRAY_QUIT: &str = "tray.quit";

    // ── 捕获/识别动态状态（spawn 闭包里产生，tt() 取词）──
    pub const STA_CAPTURING: &str = "sta.capturing";
    pub const STA_PROBE_FAIL: &str = "sta.probe_fail";
    pub const STA_CAP_OK: &str = "sta.cap_ok";
    pub const STA_CAP_FAIL: &str = "sta.cap_fail";
    pub const STA_RECOGNIZE_DONE: &str = "sta.recognize_done";
    pub const STA_RECOGNIZE_FAIL: &str = "sta.recognize_fail";
    pub const STA_OCR_THREAD_LOST: &str = "sta.ocr_thread_lost";
    pub const STA_CLIP_READING: &str = "sta.clip_reading";
    pub const STA_CLIP_OK: &str = "sta.clip_ok";
    pub const STA_CLIP_LABEL: &str = "sta.clip_label";
    pub const STA_CLIP_FAIL: &str = "sta.clip_fail";
    pub const STA_CLIP_THREAD_LOST: &str = "sta.clip_thread_lost";
    /// 历史摘要的"无文本"兜底
    pub const STA_NO_TEXT: &str = "sta.no_text";
    /// 桌面通知正文（P1 #10）
    pub const NTF_DONE: &str = "ntf.done";
    /// 错误前缀（中英文同形，独立成键保持样式判断 starts_with('❌') 一致）
    pub const ERR_MARK: &str = "err.mark";

    // ── 捕获方式 / 引擎状态标签（状态消息与设置页引擎卡共用）──
    pub const KIND_FULL: &str = "kind.full";
    pub const KIND_REGION: &str = "kind.region";
    pub const KIND_ACTIVE: &str = "kind.active";
    /// M 波：内置冻结帧选择器提交识别时的捕获方式标签
    pub const KIND_REGION_BUILTIN: &str = "kind.region_builtin";
    /// M 波：窗口选择器提交识别时的状态标签（模板，{n}=窗口标题）
    pub const WIN_LABEL_TPL: &str = "kind.window_tpl";
    pub const ENG_INIT: &str = "eng.init";
    pub const ENG_READY: &str = "eng.ready";
    pub const ENG_BUSY: &str = "eng.busy";
    pub const ENG_ERROR: &str = "eng.error";
    /// 引擎预热线程意外退出
    pub const ENG_THREAD_LOST: &str = "eng.thread_lost";
    pub const ENG_CARD_INIT: &str = "eng.card_init";
    pub const ENGINE_RUNNING: &str = "eng.running";
    pub const ENGINE_CONFIGURED: &str = "eng.configured";

    // ── 历史面板动态文案（P1 #7 + 历史落盘）──
    pub const HIST_RESTORED: &str = "hist.restored";
    pub const HIST_FULL_RESTORED: &str = "hist.full_restored";
    pub const HIST_CLEARED: &str = "hist.cleared";
    pub const HIST_LINES: &str = "hist.lines";
    pub const HIST_DEL_TIP: &str = "hist.del_tip";
    pub const HIST_MISSING: &str = "hist.missing";
    pub const COPY_ROW_TIP: &str = "copy.row_tip";
    pub const LOCATE_TIP: &str = "locate.tip";
    pub const COPY_FAIL: &str = "copy.fail";
    pub const CROP_THREAD_LOST: &str = "crop.thread_lost";

    // ── 主界面杂项动态文案 ──
    pub const STA_PROBING: &str = "sta.probing";
    pub const NO_BACKEND: &str = "sta.no_backend";
    pub const HOTKEY_PAUSED_MSG: &str = "sta.hotkey_paused";
    pub const FOOTER_PAUSED: &str = "sta.footer_paused";
    pub const SEL_UNAVAILABLE: &str = "sel.unavailable";
    pub const FOOTER_BACKEND: &str = "sta.footer_backend";

    // ── M 波：内置冻结帧选择器 / 窗口选择列表 ──
    /// 全屏选择态顶栏提示
    pub const PICKER_HINT: &str = "picker.hint";
    /// 抓取全屏冻结帧中（状态行）
    pub const PICKER_GRABBING: &str = "picker.grabbing";
    /// 取消（ESC / 选区过小）
    pub const PICKER_CANCEL: &str = "picker.cancel";
    /// 冻结帧编码 data-url 失败
    pub const PICKER_ENCODE_FAIL: &str = "picker.encode_fail";
    /// 冻结帧抓取线程意外退出
    pub const PICKER_THREAD_LOST: &str = "picker.thread_lost";
    /// 窗口选择列表标题
    pub const WINPICK_TITLE: &str = "winpick.title";
    /// 窗口选择列表空态
    pub const WINPICK_EMPTY: &str = "winpick.empty";
    /// 窗口枚举失败
    pub const WINPICK_LOAD_FAIL: &str = "winpick.load_fail";

    // ── 批量页动态状态 ──
    pub const BATCH_STATUS_SELECTED: &str = "batch.status_selected";
    /// J2 #2：文件夹递归收集完成
    pub const BATCH_FOLDER_SELECTED: &str = "batch.folder_selected";
    /// J2 #2：文件夹递归收集超上限截断提示
    pub const BATCH_FOLDER_TRUNCATED: &str = "batch.folder_truncated";
    pub const BATCH_PICK_FOLDER: &str = "batch.pick_folder";
    pub const BATCH_STATUS_RUNNING: &str = "batch.status_running";
    pub const BATCH_STATUS_DONE: &str = "batch.status_done";
    pub const BATCH_THREAD_LOST: &str = "batch.thread_lost";
    pub const EXPORT_OK: &str = "batch.export_ok";
    pub const EXPORT_FAIL: &str = "batch.export_fail";
    pub const QR_ERR_EMPTY: &str = "batch.qr_err_empty";
    pub const QR_ERR_SCALE_PARSE: &str = "batch.qr_err_scale_parse";
    pub const QR_ERR_SCALE_RANGE: &str = "batch.qr_err_scale_range";
    pub const QR_GEN_OK: &str = "batch.qr_gen_ok";
    pub const QR_GEN_FAIL: &str = "batch.qr_gen_fail";
    pub const QR_THREAD_LOST: &str = "batch.qr_thread_lost";
    pub const QR_SAVE_OK: &str = "batch.qr_save_ok";
    pub const QR_SAVE_FAIL: &str = "batch.qr_save_fail";
    pub const QR_SAVE_BAD: &str = "batch.qr_save_bad";
    pub const QR_SAVE_STALE: &str = "batch.qr_save_stale";

    // ── 设置页保存反馈 ──
    pub const SET_SAVED: &str = "set.saved";
    pub const SET_BAD_REGEX: &str = "set.bad_regex";
    pub const SET_ERR_PORT_NUM: &str = "set.err_port_num";
    pub const SET_ERR_PORT_RANGE: &str = "set.err_port_range";
    pub const SET_ERR_HOTKEY: &str = "set.err_hotkey";
    pub const SET_WORKER_OK: &str = "set.worker_ok";
    pub const SET_WORKER_FAIL: &str = "set.worker_fail";
    pub const SET_THREAD_LOST: &str = "set.thread_lost";
    pub const SET_EFFECT_TIP: &str = "set.effect_tip";
}

// ── 词条表：JSON 资源文件（ui/assets/i18n/），编译期嵌入 ────────────────────
//
// 词条不写死在代码（2026-10-27 外置）：zh-CN.json 是基准表，en-US.json 允许
// 缺词（缺词回退 zh，见 t()）。serde_json 零拷贝反序列化——HashMap<&str, &str>
// 直接借用 include_str! 的 'static 文本，无运行时分配、无额外文件依赖。
// 新增词条：往两个 JSON 加 key，并在 k 常量区加对应 pub const。

use std::collections::HashMap;
use std::sync::LazyLock;

// 值用 String（启动一次解析分配）而非零拷贝 &str 借用：JSON 值里凡带
// 转义字符（如英文引号 "\""）serde_json 就无法零拷贝借出，会直接解析失败。
static ZH: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../assets/i18n/zh-CN.json"))
        .expect("zh-CN.json 解析失败（词条文件损坏？）")
});

static EN: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../assets/i18n/en-US.json"))
        .expect("en-US.json 解析失败（词条文件损坏？）")
});

/// 基准中文表：未知 key 原样返回（方便发现漏配，语义同旧 match 版）
fn zh<'a>(key: &'a str) -> &'a str {
    ZH.get(key).map(|s| s.as_str()).unwrap_or(key)
}

/// 英文表：缺词返回 None（由 t() 回退 zh）
fn en(key: &str) -> Option<&'static str> {
    EN.get(key).map(|s| s.as_str())
}


/// 词条内 `{n}` / `{v}` 占位替换的小工具（词条是 &'static str，不能 format!）
pub fn fill(tmpl: &str, n: impl std::fmt::Display) -> String {
    tmpl.replace("{n}", &n.to_string()).replace("{v}", &n.to_string())
}

/// 多占位替换：按实参顺序填 `{0}` `{1}` `{2}`…（一个词条带 ≥3 个值时用；
/// 1-2 个占位仍走 [`fill`]）。多余的占位保持原样（方便发现模板与实参不匹配）。
pub fn filln(tmpl: &str, args: &[&dyn std::fmt::Display]) -> String {
    let mut s = tmpl.to_string();
    for (i, a) in args.iter().enumerate() {
        s = s.replace(&format!("{{{i}}}"), &a.to_string());
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// en 缺词回退 zh：en 表故意不收的 key（以及未来新增未翻译的 key）
    #[test]
    fn en_falls_back_to_zh_on_missing_word() {
        // 这两个 key 只存在于 zh 表
        assert_eq!(t("en_US", "nav.recognize"), "Recognize");
        // age.* 之外的冷门词仍可回退；用未知 key 验证兜底路径
        assert_eq!(t("en_US", "totally.unknown.key"), "totally.unknown.key");
        assert_eq!(t("zh_CN", "totally.unknown.key"), "totally.unknown.key");
        // 英文表存在但 zh 兜底同样可达（结构一致性：zh 表必须覆盖 en 表全部 key）
        for key in [
            k::NAV_RECOGNIZE, k::NAV_BATCH, k::NAV_PLUGINS, k::NAV_SETTINGS,
            k::BTN_PASTE, k::HIST_TITLE, k::SET_IGNORE, k::BATCH_QR_TITLE,
            k::LAYOUT_BY_LINE, k::STATUS_READY,
        ] {
            assert_ne!(zh(key), key, "zh 表缺词：{key}");
            assert!(en(key).is_some(), "en 表缺词：{key}");
        }
    }

    /// 未知语言（zh_TW / ja_JP 等）一律回退中文
    #[test]
    fn unknown_lang_falls_back_to_zh() {
        assert_eq!(t("zh_TW", k::NAV_RECOGNIZE), "识别");
        assert_eq!(t("ja_JP", k::BTN_COPY_ALL), "复制全部");
        assert_eq!(t("", k::NAV_BATCH), "批量");
    }

    /// en 前缀匹配：en_GB 也走英文
    #[test]
    fn lang_prefix_matching() {
        assert_eq!(t("en_GB", k::NAV_RECOGNIZE), "Recognize");
        assert_eq!(t("english", k::NAV_RECOGNIZE), "Recognize");
    }

    /// fill 替换占位符
    #[test]
    fn fill_replaces_placeholders() {
        assert_eq!(fill("识别结果（{n} 行）", 3), "识别结果（3 行）");
        assert_eq!(fill("Engine {v}", "native"), "Engine native");
    }

    /// filln 按序填 {0}{1}{2}…（状态词条带多个值时的替换工具）
    #[test]
    fn filln_fills_numbered_placeholders() {
        // 捕获成功消息的真实形状：模板 × 捕获方式标签 × 宽高
        let tmpl = t("zh_CN", k::STA_CAP_OK);
        let kind = t("zh_CN", k::KIND_REGION);
        assert_eq!(
            filln(tmpl, &[&kind, &1920u32, &1080u32]),
            "✅ 区域(原生)成功: 1920x1080，识别中…"
        );
        // 缺实参的占位保持原样（模板与实参不匹配可发现）
        assert_eq!(filln("{0}/{1}", &[&7u32]), "7/{1}");
    }

    /// 托盘 + 动态状态词条：zh / en 两语言都必须有词。en 缺词会静默回退中文，
    /// 这里直接断言 en 表已覆盖全部新键（防"托盘/状态切英文后仍是中文"回归）。
    #[test]
    fn tray_and_status_keys_exist_in_both_langs() {
        for key in [
            k::TRAY_SHOW, k::TRAY_OCR, k::TRAY_OCR_PAUSED, k::TRAY_BATCH,
            k::TRAY_PAUSE_HOTKEY, k::TRAY_QUIT,
            k::STA_CAPTURING, k::STA_PROBE_FAIL, k::STA_CAP_OK, k::STA_CAP_FAIL,
            k::STA_RECOGNIZE_DONE, k::STA_RECOGNIZE_FAIL, k::STA_OCR_THREAD_LOST,
            k::STA_CLIP_READING, k::STA_CLIP_OK, k::STA_CLIP_LABEL, k::STA_CLIP_FAIL,
            k::STA_CLIP_THREAD_LOST, k::STA_NO_TEXT, k::ERR_MARK, k::NTF_DONE,
            k::KIND_FULL, k::KIND_REGION, k::KIND_ACTIVE,
            k::ENG_INIT, k::ENG_READY, k::ENG_BUSY, k::ENG_ERROR, k::ENG_THREAD_LOST,
            k::ENG_CARD_INIT, k::ENGINE_RUNNING, k::ENGINE_CONFIGURED,
            k::HIST_RESTORED, k::HIST_FULL_RESTORED, k::HIST_CLEARED, k::HIST_LINES,
            k::HIST_DEL_TIP, k::HIST_MISSING, k::COPY_ROW_TIP, k::COPY_FAIL, k::CROP_THREAD_LOST,
            k::LOCATE_TIP,
            k::STA_PROBING, k::NO_BACKEND, k::HOTKEY_PAUSED_MSG, k::FOOTER_PAUSED,
            k::SEL_UNAVAILABLE, k::FOOTER_BACKEND,
            k::BATCH_STATUS_SELECTED, k::BATCH_STATUS_RUNNING, k::BATCH_STATUS_DONE,
            k::BATCH_THREAD_LOST, k::EXPORT_OK, k::EXPORT_FAIL, k::QR_ERR_EMPTY,
            k::QR_ERR_SCALE_PARSE, k::QR_ERR_SCALE_RANGE, k::QR_GEN_OK, k::QR_GEN_FAIL,
            k::QR_THREAD_LOST, k::QR_SAVE_OK, k::QR_SAVE_FAIL, k::QR_SAVE_BAD, k::QR_SAVE_STALE,
            k::SET_SAVED, k::SET_BAD_REGEX, k::SET_ERR_PORT_NUM, k::SET_ERR_PORT_RANGE,
            k::SET_ERR_HOTKEY, k::SET_WORKER_OK, k::SET_WORKER_FAIL, k::SET_THREAD_LOST,
            k::SET_EFFECT_TIP,
            // J2 新词条：公式模式 / 提示音 / 文件夹递归
            k::FORMULA_TOGGLE, k::FORMULA_HINT, k::STA_FORMULA_INIT,
            // O 波新词条：公式引擎启动预热 / 识别耗时展示
            k::STA_FORMULA_WARMING, k::STA_FORMULA_WARMED,
            k::SET_SOUND, k::SET_SOUND_HINT,
            k::BATCH_PICK_FOLDER, k::BATCH_FOLDER_SELECTED, k::BATCH_FOLDER_TRUNCATED,
            // Wave3 新词条：无边框窗口 / 自定义标题栏
            k::SET_FRAMELESS, k::SET_FRAMELESS_HINT, k::TB_MINIMIZE, k::TB_HIDE,
            // M 波新词条：内置冻结帧选择器 / 窗口选择列表 / 选区方式
            k::KIND_REGION_BUILTIN, k::WIN_LABEL_TPL,
            k::PICKER_HINT, k::PICKER_GRABBING, k::PICKER_CANCEL,
            k::PICKER_ENCODE_FAIL, k::PICKER_THREAD_LOST,
            k::WINPICK_TITLE, k::WINPICK_EMPTY, k::WINPICK_LOAD_FAIL,
            k::SET_PICKER, k::SET_PICKER_HINT, k::SET_PICKER_AUTO,
            k::SET_PICKER_NATIVE, k::SET_PICKER_BUILTIN,
        ] {
            assert_ne!(zh(key), key, "zh 表缺词：{key}");
            assert!(en(key).is_some(), "en 表缺词：{key}");
        }
    }

    /// 全局语言原子：set_lang 后 tt()（spawn 闭包用的无参取词）立即生效
    #[test]
    fn set_lang_switches_atomic_lookup() {
        set_lang("zh_CN");
        assert_eq!(lang(), "zh_CN");
        assert_eq!(tt(k::TRAY_QUIT), "退出");
        assert_eq!(tt(k::TRAY_SHOW), "打开主窗口");

        set_lang("en_US");
        assert_eq!(lang(), "en_US");
        assert_eq!(tt(k::TRAY_QUIT), "Quit");
        assert_eq!(tt(k::TRAY_SHOW), "Open main window");
        assert_eq!(tt(k::TRAY_OCR), "Screenshot OCR ({n})"); // 未替换的占位原样返回

        // 未知语言 → 回中文基准
        set_lang("zh_TW");
        assert_eq!(lang(), "zh_CN");
        assert_eq!(tt(k::TRAY_QUIT), "退出");
        set_lang("zh_CN");
    }
}
