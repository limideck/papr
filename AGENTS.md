# AGENTS.md

Papr — local-first RSS/新闻阅读器。Rust 核心（库无 UI）+ agent CLI + Tauri/web server，单 SQLite 数据库，musl 静态单二进制，systemd 部署。标签与摘要使用外部 LLM API（如 DeepSeek），不内置模型。

## 工作区结构

- `crates/papr-core/` — UI-free 核心：DB、抓取、AI 打标/摘要、翻译、搜索、标签治理。**所有业务逻辑都在这里。**
- `crates/papr-cli/` — agent/自动化 CLI（clap），输出 TOON/JSON。`src/main.rs`。
- `crates/papr-server/` — Tauri/桌面端（需要系统库 glib/pkg-config，纯沙箱无 GUI 依赖时可能编不过，与 core/cli 无关）。

## 构建与测试

需 Rust（环境变量：`RUSTUP_HOME` / `CARGO_HOME` / `PATH` 指向 `~/.cargo/bin`）。包管理/构建用 cargo（Rust 项目不用 pnpm）。

```bash
# 跑核心全部单元测试
cargo test -p papr-core

# 只跑标签治理测试
cargo test -p papr-core tag_taxonomy

# CLI 类型检查 + 测试
cargo check -p papr-cli
cargo test -p papr-cli
```

> `crates/papr-server` 依赖 glib-2.0（pkg-config），无桌面系统库的环境下 `cargo build --workspace` 会失败——属环境限制，不是 core/cli 回归。验证 core + cli 即可。

## 关键模块

### 标签系统（tags / tag_aliases）

- 三套既有词表：`tags(kind='interest')`（封闭）、`tags(kind='ai')`（自由生成，易碎片化）、wordcloud 实体（库外 JSON）。
- `tag_aliases`（v30 迁移）：`UNIQUE(kind, alias COLLATE NOCASE)`，打标时 `resolve_tag_by_name_or_alias` 优先解析；当前不参与 FTS。
- 迁移是**追加式**：`crates/papr-core/src/db.rs` 的 `MIGRATIONS` vec，只在末尾加 `M::up(...)`，不改历史。

### 标签治理（本项目新增 `crates/papr-core/src/tag_taxonomy.rs`）

- v32 迁移：`tags.parent_id`（`ON DELETE SET NULL`，单父两层深）、`tags.tag_type`（`entity`/`topic`）、`idx_tags_parent`。
- 策略：**同义词横向合并**（中东/中东局势/Middle East → 一个规范标签，旧名钉为别名防复发）；**相关实体纵向分层**（伊朗是 entity，挂到 topic 下，**不** merge）。
- `deterministic_groups`：按 `surface_key`（lowercase + 仅字母数字）折叠标点/空格/连字符变体，零成本确定性合并。
- `build_plan`：先确定性分组，再对用量 ≥ `min_count` 的长尾分批调 LLM 聚类（每批带 top-3 文章标题消歧），feature=`tag-tidy` 计费到 `ai_usage`；坏批次 warn 跳过。
- `apply_plan`：先 `merge_tags_keep_alias` 合并组（旧名写入 `tag_aliases`），再设层级/类型。
- CLI：
  - `papr tag tidy` — 盘点统计 + 产出 JSON 计划（默认不改库）；`--apply --yes` 直接落地。
  - `papr tag apply <PLAN_FILE|-> --yes` — 应用人工审核/编辑后的计划（`-` 从 stdin 读）。
- 写入侧防复发：`auto_tag.rs::apply_ai_tags` 写入前先 `resolve_ai_tag_for_writing`（精确名/别名 → surface 变体扫描），命中复用已有 tag，只有真未知才 `create_tag`。

## 代码风格

- Rust 2021，clap derive 定义 CLI，rusqlite + rusqlite_migration。
- DB 错误统一用 `AppResult`/`AppError`；CLI 层用 `AxiError`（`db_err` / `clean_err` 转换）。
- 测试用内存 SQLite：`db::migrate_connection(&mut conn)`（公开）跑迁移后再操作。
- **时间相关测试禁忌**：不要在测试夹具里写死绝对日期（如 `2026-08-01`），backfill 窗口用 `datetime('now','-N days')` 动态生成，否则随真实时钟过期。
