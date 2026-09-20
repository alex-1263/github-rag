//! CLI 薄壳:参数解析 → core。业务逻辑为零(AGENTS.md 架构规则)。

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "gh-rag", version, about = "semantic memory over GitHub issues")]
struct Args {
    /// 里程碑占位:M1 只提供 self-check,M3 补全全部子命令
    #[command(subcommand)]
    _cmd: Option<Cmd>,
}

#[derive(clap::Subcommand, Debug)]
enum Cmd {
    /// M1:验证 ONNX 环境可用(黄金对齐的本地快捷入口)
    Doctor,
}

fn main() -> anyhow::Result<()> {
    let _args = Args::parse();
    println!("gh-rag (rust) — M1 scaffold; see DESIGN.md milestones");
    Ok(())
}
