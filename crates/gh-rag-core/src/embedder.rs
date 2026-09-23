//! Embedder trait:向量生产者的唯一抽象。
//!
//! 实现(fastembed 封装,内部即 ort + tokenizers):
//! - `FastEmbedder`:本地 bge-m3(int8),dense 输出 + L2 归一化。
//!   与 Python sentence-transformers(fp32)的对齐度由黄金测试度量。

use crate::Result;

/// 环境指纹:钉进 index.sqlite 的 manifest,防止向量空间混用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingFingerprint(pub String);

/// 嵌入文本组装:标题重复加权 + 正文截断。
/// 必须与 Python 版 `BgeM3Embedder.build_text` 逐字符一致(有单元测试)。
pub fn build_text(title: &str, body: &str, title_repeats: usize, body_max_chars: usize) -> String {
    let t = title.trim();
    let head = format!("{}\n", t).repeat(title_repeats);
    let body_prefix: String = body.chars().take(body_max_chars).collect();
    format!("{}{}", head, body_prefix)
}

pub trait Embedder {
    /// 批量嵌入(建库路径)。输出 float32 小端字节,1024 维,L2 归一化。
    fn embed_texts(&self, texts: &[String]) -> Result<Vec<Vec<u8>>>;

    /// 单条查询嵌入(检索路径)。返回 1024 维 f32,L2 归一化。
    fn embed_query(&self, text: &str) -> Result<Vec<f32>>;

    /// 环境指纹(模型+实现+序列长度),写入 manifest。
    fn fingerprint(&self) -> EmbeddingFingerprint;
}

/// 评论聚合版嵌入文本:标题加权 + 正文截断 + 评论区(独立配额,时间序)。
/// 总量按模型窗口预算(qwen3.7 128K / bge-m3 8K,均远大于此处上限)。
/// bot 过滤由调用方(sync)负责,本函数保持纯。
pub fn build_text_with_comments(
    title: &str,
    body: &str,
    comments: &[(&str, &str)], // (author, body) 时间序
    title_repeats: usize,
    body_max_chars: usize,
    comment_quota: usize,
    per_comment_max: usize,
) -> String {
    let mut text = build_text(title, "", title_repeats, 0); // 标题段(自带换行)
    text.push_str(&body.chars().take(body_max_chars).collect::<String>());
    if comments.is_empty() {
        return text;
    }
    text.push_str("\n\n[讨论]\n");
    let mut used = 0usize;
    for (author, cbody) in comments {
        if used >= comment_quota {
            break;
        }
        let line: String = format!(
            "[- {}] {}\n",
            author,
            cbody.chars().take(per_comment_max).collect::<String>()
        );
        let line_chars = line.chars().count();
        if used + line_chars > comment_quota {
            let room = comment_quota.saturating_sub(used);
            text.push_str(&line.chars().take(room).collect::<String>());
            break;
        }
        text.push_str(&line);
        used += line_chars;
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_aggregate_within_quota() {
        let t = build_text_with_comments(
            "标题",
            "正文",
            &[("a", "评论一"), ("b", "评论二")],
            2,
            100,
            1000,
            500,
        );
        assert!(t.contains("标题\n标题\n"));
        assert!(t.contains("[讨论]"));
        assert!(t.contains("[- a] 评论一"));
        assert!(t.contains("[- b] 评论二"));
    }

    #[test]
    fn comment_quota_truncates_tail() {
        let t = build_text_with_comments(
            "t",
            "b",
            &[("a", "很长".repeat(300).as_str())],
            1,
            10,
            100,
            500,
        );
        assert!(t.chars().count() < 200, "配额 100 应截断超长评论");
    }

    #[test]
    fn no_comments_keeps_plain_form() {
        let t = build_text_with_comments("标题", "正文", &[], 2, 100, 100, 100);
        assert_eq!(t, "标题\n标题\n正文");
    }
}
