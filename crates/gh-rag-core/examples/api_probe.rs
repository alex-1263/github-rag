//! 401 诊断:打印硅基流动 /embeddings 的完整响应体(定位 key/网络/平台问题)。
//! 用法:GH_RAG_API_KEY=xxx cargo run -p gh-rag-core --example api_probe --release

fn main() {
    let key = std::env::var("GH_RAG_API_KEY").expect("GH_RAG_API_KEY required");
    println!("key_len={}", key.len());
    let agent = ureq::AgentBuilder::new().build();
    let r = agent
        .post("https://api.siliconflow.cn/v1/embeddings")
        .set("Authorization", &format!("Bearer {key}"))
        .set("Content-Type", "application/json")
        .send_string(r#"{"model":"BAAI/bge-m3","input":["test"]}"#);
    match r {
        Ok(resp) => {
            println!("STATUS: {}", resp.status());
            match resp.into_string() {
                Ok(b) => println!("BODY: {}", b.chars().take(400).collect::<String>()),
                Err(e) => println!("BODY ERR: {e}"),
            }
        }
        Err(ureq::Error::Status(code, resp)) => {
            println!("STATUS: {code}");
            let hdrs: Vec<String> = resp
                .headers_names()
                .iter()
                .filter_map(|n| resp.header(n).map(|v| format!("  {n}: {v}")))
                .collect();
            println!("HEADERS:\n{}", hdrs.join("\n"));
            match resp.into_string() {
                Ok(b) => println!("BODY: {}", b.chars().take(400).collect::<String>()),
                Err(e) => println!("BODY ERR: {e}"),
            }
        }
        Err(e) => println!("TRANSPORT ERR: {e}"),
    }
}
