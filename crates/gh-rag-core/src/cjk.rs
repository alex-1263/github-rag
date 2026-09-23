//! CJK 文本 FTS5 预处理:unicode61 把连续 CJK 串当一个 token,
//! 中文子串永远查不到。游程切重叠双字组(2-gram)即可让 FTS5 逐词索引。

/// 连续 CJK 字符游程切为重叠双字组,以空格分隔(游程长 1 保留单字;非 CJK 原样)。
pub fn cjk_bigram(text: &str) -> String {
    text.to_string()
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
