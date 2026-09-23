//! CJK 文本 FTS5 预处理:unicode61 把连续 CJK 串当一个 token,
//! 中文子串永远查不到。游程切重叠双字组(2-gram)即可让 FTS5 逐词索引。

/// 判断是否 CJK 表意字符(汉字、假名、谚文——这些在 unicode61 下
/// 连续成串时会被当作单一 token,导致子串永远查不到)。
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF   // 平假名/片假名
        | 0x3400..=0x4DBF // CJK 扩展 A
        | 0x4E00..=0x9FFF // CJK 统一表意
        | 0xAC00..=0xD7AF // 谚文音节
        | 0xF900..=0xFAFF // CJK 兼容表意
        | 0x20000..=0x2FA1F // CJK 扩展 B..F + 兼容
    )
}

/// 连续 CJK 字符游程切为重叠双字组,以空格分隔(游程长 1 保留单字;非 CJK 原样)。
pub fn cjk_bigram(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut parts: Vec<String> = Vec::new();
    let mut run: Vec<char> = Vec::new();
    for &c in &chars {
        if is_cjk(c) {
            run.push(c);
            continue;
        }
        flush_run(&mut run, &mut parts);
        // 非 CJK 字符并入最后一个非 CJK 分段(原样保留内部空白);
        // 分段边界空白不落盘——join 的分隔符已表达切分语义
        match parts.last_mut() {
            Some(p) if !p.chars().next_back().is_some_and(is_cjk) => p.push(c),
            _ if c.is_whitespace() => {}
            _ => parts.push(c.to_string()),
        }
    }
    flush_run(&mut run, &mut parts);
    // 分段内部空白原样保留,仅去掉切分边缘的空白(join 已表达分隔)
    parts
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// 游程落盘:空跳过;单字保留;多字切重叠双字组。
fn flush_run(run: &mut Vec<char>, parts: &mut Vec<String>) {
    match run.len() {
        0 => {}
        1 => parts.push(run.remove(0).to_string()),
        _ => {
            for w in run.windows(2) {
                parts.push(w.iter().collect());
            }
            run.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::cjk_bigram;

    #[test]
    fn pure_chinese_splits_into_bigrams() {
        assert_eq!(cjk_bigram("乱码"), "乱码");
        assert_eq!(cjk_bigram("中文乱码"), "中文 文乱 乱码");
    }

    #[test]
    fn mixed_cjk_and_ascii() {
        assert_eq!(cjk_bigram("导出 CSV 中文乱码"), "导出 CSV 中文 文乱 乱码");
    }

    #[test]
    fn pure_ascii_untouched() {
        assert_eq!(cjk_bigram("export CSV broken"), "export CSV broken");
    }

    #[test]
    fn single_cjk_char_kept() {
        assert_eq!(cjk_bigram("码"), "码");
    }

    #[test]
    fn empty_string() {
        assert_eq!(cjk_bigram(""), "");
    }
}
