//! ==== 内嵌前端资源 ====
//! 内嵌的单文件 Web 管理端（运行时通过内存直接服务，也可用 `export-web` 导出）。

/// 编译期把 web/index.html 打进二进制，部署时无需附带前端文件。
pub const INDEX_HTML: &str = include_str!("../web/index.html");