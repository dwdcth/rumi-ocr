# 提案：表格进翻译流（docforge IR/单元/回写 三层改造草案）

> 来源：rumi 接入方视角（2026-11-04），基于 docforge@82cdbb46 源码实测。
> 验收标准写死在 §1：**rumi 侧零代码改动**就能翻表格——所有结构演化都在 docforge 内闭环。

## 0. 现状证据（为什么成本低）

- **`TableData { rows: Vec<Vec<String>>, header_rows: usize }` 已经存在**（`model.rs:70`，
  serde 就绪含 wire 兼容的 `skip_serializing_if`），挂在 `RawTextItem.table: Option<TableData>`
  （`plugin.rs:41`）——**解析侧行列结构是现成的**（docx/odt/xlsx/pptx 表格全走
  `table_item[_with_header]`）。
- 断点在 `OfficePlugin::assemble`：表格项 → `BlockKind::Table` 时 **TableData 被丢弃**，
  content 退化为「表头 `" | "` 连接」的摘要文本（office/mod.rs:831 注释与测试锁定该行为）。
- 消费侧三处排除：`emit::is_translatable(Table) = false`；`to_translation_units` 白名单跳过；
  `writeback::docx` 模块头明确「表格内 `w:p` 不占翻译位，遇 `w:tbl` 整棵子树跳过」。
- 用户痛感：xlsx/docx 的表格内容（参数表/对照表/数据字典）一个字不翻——
  「翻译整份文档」的高频刚需缺口。

## 1. 验收标准（rumi 消费视角，先写死）

```rust
// rumi 现有管线（translate/src/docmt.rs）不改一行：
let doc = docforge::parse("spec.docx")?;
let units = docforge::emit::to_translation_units(&doc);
// units 现在自动包含 p0-b2-r1c3 这类表格单元格条目（text="额定电压"）
let translated: Vec<(String, String)> = units.iter()
    .map(|u| (u.id.clone(), my_translate(&u.text)))
    .collect();
docforge::writeback::writeback_docx(in, &translated, BiStrategy::TranslationOnly, out)?;
// out 里该单元格文本已替换为译文，表格样式/列宽/边框原样保留
```

## 2. IR 层：`Block.table` 透传（改动最小的一层）

```rust
pub struct Block {
    // ...现有字段不动
    /// 表格块的结构化数据（kind == Table 时由 assemble 从 RawTextItem.table
    /// 透传；其他 kind 恒 None）。wire：None 不出键，旧 JSON 读入默认 None
    /// （先例：Block.quad 的 wire 兼容口径）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table: Option<TableData>,
}
```

- `TableData` 本体不动（rows 已是物理网格：docx 的 gridSpan/vMerge 跨度在解析侧
  已按网格对齐——续格空串；xlsx 合并单元格同口径。**无需新增 span 字段**，
  空串格天然被翻译流跳过）。
- `assemble` 侧：Table 块的 `content` 保持现状（表头摘要——文本消费者/全文检索
  的兼容面不动），`table` 字段并行携带结构。**一处透传，两种消费者各取所需。**

## 3. 单元层：单元格展开与 id 扩展

### id 形态

```
普通块：  p{页}-b{块序}              （现状，不动）
表格单元格：p{页}-b{块序}-r{行}c{列}   （新增，前缀是所在 Table 块的家系）
```

前缀兼容性：`writeback_*` 的 translation_map 按 id 精确匹配，`p0-b2` 与
`p0-b2-r1c3` 不冲突；同一文档混排两类 id 天然共存。

### `to_translation_units` 改动

```rust
// 伪码：块循环内，Table 分支从"跳过"改为"展开"
BlockKind::Table => {
    let Some(td) = &block.table else {
        continue; // 无结构数据的表格（理论不出现，防御）维持现状跳过
    };
    for (r, row) in td.rows.iter().enumerate() {
        for (c, cell) in row.iter().enumerate() {
            let text = cell.trim();
            if text.chars().count() < min_chars { continue; }   // 同款判据
            units.push(TranslationUnit {
                id: format!("{base}-r{r}c{c}"),
                kind: BlockKind::Table,     // kind 不新增，按 id 形态可识别
                text: text.to_string(),
                source: block.source, confidence: block.confidence,
                lang: block.lang, /* page/chapter 同块 */,
            });
        }
    }
}
```

- `is_translatable(Table)`：**语义改为 true**（该函数同时被 markdown 回写的
  过滤口径使用——配套改动见 §5）。为外部消费者的过渡，可在文档里注明
  「Table=true 指单元格级展开，整块文本仍以 content 摘要为准」。
- min_chars 对表格的特殊性：`min_chars=2` 会漏掉单字符单元格（"是/否"、单位
  "V"/"A"）——**建议 Table 单元格用 `max(1, min_chars)` 口径**并在 doc 注明
  （数字/纯符号格翻译引擎自然原样返回，无害）。

### 进度语义（rumi 消费提醒，docforge 无需改）

单元格单元数量可能很大（大表 500+ 格）——rumi 的 16 单元/批进度节流不受影响，
但 `DocStats.units` 会显著增大。这是预期行为，不是回归。

## 4. 回写层：docx/odt/html/epub 格内替换

### docx（先行格式）

对齐机制与段落位完全同构，只是扫描目标换成表格：

- XML 扫描：`w:tbl` → 逐 `w:tr`（行）→ 逐 `w:tc`（格）→ 格内首 `w:p` 为该格
  的文本载体（多段落格 v1 只写首段，格内多段落在 doc 解析侧本就 join 成一格文本）。
- **网格对齐**：`w:tc` 带 `gridSpan` 时该格占多列——扫描器列游标按 span 推进
  （与解析侧 `table_item` 的网格构建同款规则，确定性对齐）。
- id 回填：第 k 个 `w:tbl` ↔ 第 k 个 Table 块（同「第 k 个段落位」机制），
  格文本与重解析 TableData.rows[r][c] **文本一致性双保险**（不一致跳过该格，
  宁可漏写不错位写——与段落守卫同款）。
- 策略语义（表格内的特殊口径，模块头注明）：
  - `TranslationOnly`（**表格推荐**，rumi 侧会在文档翻译提示）：格内首 `w:t`
    换译文、其余 `w:t` 清空（与段落 TranslationOnly 同款简化）。
  - `BilingualInline`：格内在原 `w:p` 后**追加一个新 `w:p`** 放译文（复制该格
    `w:pPr`；不软换行内联——`w:br` 会破 docx 表格行的行高语义）。
  - `OriginalPlusAppendix`：表格不进附录（附录只收段落/标题单元），表格保持原样
    ——语义一致性代价最小。
- odt（`table:table` → `table:table-row` → `table:table-cell`）：同构，第二优先。

### html / epub（已是回写格式）

- `epub`：xhtml 的 `<table><tr><td>` 树扫描，格内文本节点替换；
  `BilingualInline` 格内用 `<br class="docforge-tr"/>` 分隔原/译（沿用
  `docforge-tr` class 家族便于下游识别）。
- `html`：同 epub。
- 注意：epub 回写的 spine 对齐机制不变（表格在部件内，页/块序不受影响）。

### xlsx（暂不承诺，另案）

xlsx 回写器本就不存在（七格式回写无 xlsx）。表格翻译对 xlsx 的完整闭环 =
xlsx 回写器（sharedStrings/inlineStr 单元格替换 + cell ref 换算），工作量
独立成项，不阻塞本提案（docx/odt/html/epub 先行）。

### markdown 回写

`to_bilingual_markdown` 的表格渲染：块有序列里有 `pN-bM-rXcY` 条目时，该表
按「译后网格」渲染 markdown 表格（`|` 分隔，TranslationOnly 语义直接换格文本；
Inline 语义格内 `<br>`——markdown 表格内换行本就用 `<br>`）；无条目保持现状
（content 摘要渲染）。

## 5. 兼容与迁移

| 面 | 影响 | 处置 |
| --- | --- | --- |
| Block JSON wire | 新增可选键 `table` | None 不出键、旧 JSON 读入 None（quad 先例） |
| `is_translatable(Table)` | false→true | docforge 年轻期直接演进 + changelog 注明；markdown 回写同步适配（§4） |
| 现有 writeback 用户 | 无（此前表格根本不进翻译流，translated 里不会有表格 id） | 空 map 天然 no-op |
| 解析侧 | 零改动（TableData 已在 Raw 层） | 仅 assemble 透传 |
| rumi 消费 | **零改动**（§1 验收） | 无 |

渐进顺序：① Block.table 透传 + 单测 → ② 单元展开（+markdown 回写适配）→
③ docx 回写格替换 → ④ html/epub → ⑤ odt。每步独立可测可发布。

## 6. 边界与开放问题

1. **嵌套表**（docx 表格套表格）：解析侧现状如何产（外层一格含内表？）待核；
   v1 口径：内表格按外层格文本的一部分（join），不单独展开——与现状一致。
2. **格内富文本**（上下标/加粗混排）：v1 格级替换丢格式（与段落 TranslationOnly
   的简化同款）；「保留 run 结构的智能替换」留 v2（首 run 换文本、余 run 清空
   的折中已在段落侧有先例）。
3. **表头是否跳过翻译**：不跳（表头常常正是最需要翻的）；想跳过的消费者
   按 `header_rows` 自行过滤（TranslationUnit 无此字段——可选：单元 meta 带
   `header: bool`，**建议加**，成本一行）。
4. 超长格文本（格内多段）已由解析侧 join 承载，回写只写回首段——语义在
   doc 注明「格内多段落合并为单段译文」。

## 7. 工作量评估（docforge 侧）

| 步骤 | 估量 |
| --- | --- |
| ① Block.table 透传 + 单测 | ~0.5 天 |
| ② 单元展开 + markdown 适配 + 单测 | ~1 天 |
| ③ docx 回写格替换（含 gridSpan 对齐与双保险）+ 金测 | ~1.5 天 |
| ④ html/epub 格替换 | ~1 天 |
| ⑤ odt | ~0.5 天 |

合计 ~4.5 天，全部渐进可发布。③ 完成即可满足 rumi 最高频场景（docx 报告/规格书翻译）。
