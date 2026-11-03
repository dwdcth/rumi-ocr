//! lang / segment / postfix 纯逻辑单测（规格 §2/§3/§4 用例期望值逐条写死）。

use rumi_translate::lang;
use rumi_translate::postfix::polish_zh;
use rumi_translate::segment::{segment, segment_lines};

// ---------- lang::normalize（规格 §2：≥12 条，含大小写/分隔符/中文写法） ----------

#[test]
fn normalize_zh_variants() {
    assert_eq!(lang::normalize("zh-CN"), "zh");
    assert_eq!(lang::normalize("zh_CN"), "zh");
    assert_eq!(lang::normalize("zh-Hans"), "zh");
    // 繁体独立内部码 zht（NLLB zho_Hant，2026-11-03 扩容起生效）
    assert_eq!(lang::normalize("zh-TW"), "zht");
    assert_eq!(lang::normalize("zh-Hant"), "zht");
    assert_eq!(lang::normalize("zht"), "zht");
    assert_eq!(lang::normalize("cht"), "zht");
    assert_eq!(lang::normalize("繁体中文"), "zht");
    assert_eq!(lang::normalize("zhs"), "zh");
    assert_eq!(lang::normalize("chs"), "zh");
    assert_eq!(lang::normalize("chinese"), "zh");
    assert_eq!(lang::normalize("中文"), "zh");
    assert_eq!(lang::normalize(" 中文 "), "zh");
}

#[test]
fn normalize_en_variants() {
    assert_eq!(lang::normalize("en-US"), "en");
    assert_eq!(lang::normalize("en_GB"), "en");
    assert_eq!(lang::normalize("eng"), "en");
    assert_eq!(lang::normalize("english"), "en");
    assert_eq!(lang::normalize("英文"), "en");
    assert_eq!(lang::normalize("EN"), "en");
}

#[test]
fn normalize_other_codes() {
    assert_eq!(lang::normalize("JA"), "ja"); // 大小写兜底
    assert_eq!(lang::normalize("ja-JP"), "ja");
    assert_eq!(lang::normalize("jpn"), "ja");
    assert_eq!(lang::normalize("日语"), "ja");
    assert_eq!(lang::normalize("kor"), "ko");
    assert_eq!(lang::normalize("fra"), "fr");
    assert_eq!(lang::normalize("deu"), "de");
    assert_eq!(lang::normalize("spa"), "es");
    assert_eq!(lang::normalize("es-419"), "es");
    assert_eq!(lang::normalize("rus"), "ru");
    assert_eq!(lang::normalize("PT"), "pt");
    assert_eq!(lang::normalize("it"), "it");
    assert_eq!(lang::normalize("vi"), "vi");
    assert_eq!(lang::normalize("ar"), "ar");
    assert_eq!(lang::normalize("th"), "th");
    assert_eq!(lang::normalize("xx"), "xx"); // 未知原样返回小写
}

#[test]
fn display_names() {
    assert_eq!(lang::display("zh"), "中文");
    assert_eq!(lang::display("en"), "英语");
    assert_eq!(lang::display("ja"), "日语");
    assert_eq!(lang::display("xx"), "xx"); // 未收录 → 原码
}

#[test]
fn ui_langs_order_fixed() {
    // 常用组顺序写死（2026-11-03 扩容：+zht 繁体中文）
    assert_eq!(
        lang::UI_LANGS,
        &["zh", "zht", "en", "ja", "ko", "fr", "de", "es", "ru", "pt", "it", "vi", "ar", "th"]
    );
    // 更多语言组：50 种，首尾钉死 + 总数不变式
    assert_eq!(lang::UI_LANGS_MORE.len(), 50);
    assert_eq!(lang::UI_LANGS_MORE.first(), Some(&"hi"));
    assert_eq!(lang::UI_LANGS_MORE.last(), Some(&"bo"));
    // 两组无交集，且每码都有 NLLB 码（覆盖一致性）
    for &l in lang::UI_LANGS_MORE.iter() {
        assert!(!lang::UI_LANGS.contains(&l), "{l} 两组重复");
    }
}

#[test]
fn nllb_codes_cover_all_ui_langs() {
    // 每个下拉候选都必须有 NLLB 码（下拉可选 ⇔ NLLB 可翻；写错码会静默失败）
    for &l in lang::UI_LANGS.iter().chain(lang::UI_LANGS_MORE.iter()) {
        assert!(rumi_translate::engine::nllb_code(l).is_some(), "{l} 缺 NLLB 码");
    }
    assert_eq!(rumi_translate::engine::nllb_code("zht"), Some("zho_Hant"));
    assert_eq!(rumi_translate::engine::nllb_code("eo"), None);
}

// ---------- segment（规格 §3 用例表，逐条写死） ----------

#[test]
fn segment_cjk_terminator() {
    assert_eq!(
        segment("默认端口为8080。请先设置环境变量。"),
        vec!["默认端口为8080。", "请先设置环境变量。"]
    );
}

#[test]
fn segment_latin_keeps_leading_space() {
    // 第二段带前导空格，保留原样
    assert_eq!(segment("Set X. Then start it!"), vec!["Set X.", " Then start it!"]);
}

#[test]
fn segment_consecutive_terminators_merge_front() {
    assert_eq!(segment("第一句！！第二句？"), vec!["第一句！！", "第二句？"]);
}

#[test]
fn segment_no_terminal_no_split() {
    assert_eq!(segment("one,two,three"), vec!["one,two,three"]);
}

#[test]
fn segment_dot_inside_identifier_not_split() {
    // ASCII '.' 下一字符是字母/数字 → 不是句界：IP、文件名、版本号必须整段保留
    assert_eq!(segment("127.0.0.1 is the host."), vec!["127.0.0.1 is the host."]);
    assert_eq!(segment("Check app.log for details."), vec!["Check app.log for details."]);
    assert_eq!(segment("Requires v1.2.3 or newer."), vec!["Requires v1.2.3 or newer."]);
    // 对照：句尾 '.' 后是空格/EOL → 正常断句
    assert_eq!(segment("Set X. Then start it!"), vec!["Set X.", " Then start it!"]);
}

#[test]
fn segment_dot_before_digit_sentence_still_splits() {
    // "8080. Set"——'.' 后是空格，是正常句界（数字在前不豁免）
    assert_eq!(
        segment("The default port is 8080. Set the variable first."),
        vec!["The default port is 8080.", " Set the variable first."]
    );
}

#[test]
fn segment_long_line_all_under_300_and_lossless() {
    // 600 字中文逗号串：无终结符，仅次级标点二分；拼回原文等长
    let text = "甲，乙，丙，".repeat(100);
    let segs = segment(&text);
    assert!(!segs.is_empty());
    for s in &segs {
        assert!(s.chars().count() <= 300, "段超长：{} chars", s.chars().count());
    }
    assert_eq!(segs.concat(), text);
}

#[test]
fn segment_short_line() {
    assert_eq!(segment("你好"), vec!["你好"]);
}

#[test]
fn segment_empty_text() {
    assert_eq!(segment(""), Vec::<String>::new());
}

#[test]
fn segment_lines_keep_line_structure() {
    assert_eq!(
        segment_lines("第一行。\nsecond line\n"),
        vec![
            vec!["第一行。".to_string()],
            vec!["second line".to_string()],
            vec![String::new()],
        ]
    );
}

#[test]
fn segment_lines_blank_line_yields_empty_segment() {
    // 规格修正：空/纯空白行产出 [""] 一个空串段
    assert_eq!(
        segment_lines("a。\n  \nb。"),
        vec![vec!["a。".to_string()], vec![String::new()], vec!["b。".to_string()]]
    );
}

#[test]
fn segment_long_without_punct_stays_whole() {
    // 规则 3 终止条件：无标点可切 → 超长段原样保留
    let text = "字".repeat(600);
    assert_eq!(segment(&text), vec![text.clone()]);
}

// ---------- postfix::polish_zh（规格 §4 用例逐条写死） ----------

#[test]
fn r0_sentencepiece_marker_stripped() {
    // 实测译文开头泄漏 "▁"（U+2581）→ R0 还原空格并去首尾；行尾 "量 。" 由 R4 清理
    assert_eq!(
        polish_zh("▁在启动 Docker 容器前设定环境变量 。"),
        "在启动 Docker 容器前设定环境变量。"
    );
}

#[test]
fn r1_dot_digit_protection() {
    // 前是数字 → 不转；版本号（前后均为字母数字）→ 不转
    assert_eq!(polish_zh("8080."), "8080.");
    assert_eq!(polish_zh("v1.2.3"), "v1.2.3");
}

#[test]
fn r1_dot_converts_plain_tail() {
    // 后是 EOL / 空格 → 转
    assert_eq!(polish_zh("Docker."), "Docker。");
    assert_eq!(polish_zh("Done. OK"), "Done。 OK");
}

#[test]
fn r2_comma_digit_protection() {
    assert_eq!(polish_zh("1,000"), "1,000");
    assert_eq!(polish_zh("a, b"), "a， b");
}

#[test]
fn r3_digit_colon_space_removed() {
    assert_eq!(polish_zh("8: 15"), "8:15");
    // R1 已保住 IP 的点，R3 去掉数字冒号空格
    assert_eq!(polish_zh("127.0.0.1: 8080"), "127.0.0.1:8080");
}

#[test]
fn r4_space_before_punct_removed() {
    assert_eq!(polish_zh("30 秒 。"), "30 秒。");
}

#[test]
fn r5_space_after_sentence_end_before_cjk_removed() {
    assert_eq!(polish_zh("。 下一步"), "。下一步");
    assert_eq!(polish_zh("。 Next"), "。 Next"); // 后非 CJK 不动
}
