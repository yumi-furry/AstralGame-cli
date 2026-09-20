//! 内嵌的单文件 Web 管理端（运行时通过内存直接服务，也可用 `export-web` 导出）。
pub const INDEX_HTML: &str = include_str!("../web/index.html");