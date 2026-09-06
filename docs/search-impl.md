# 搜索实现总结

> 面向实现者的架构说明。查询语言的**语义契约**见 [`search.md`](search.md)（运算符、模式、字段），中英同义词展开的设计见 [`search-synonyms.md`](search-synonyms.md)。本文讲"代码里到底怎么跑"。

## 1. 引擎定位

- **纯 SQLite FTS5 关键词搜索**，无 Elasticsearch / Meilisearch；历史上曾有 sqlite-vec 语义搜索，已移除（迁移 v2 仅占位保留版本号）。
- 索引内容：`title` + `body`（正文纯文本）两列；**不索引** `summary` / `content_html` / `tag` 名称 / 作者。

## 2. 索引层（db.rs）

### 建表（迁移 v1）

```sql
CREATE VIRTUAL TABLE articles_fts USING fts5(
    title, body, tokenize = 'porter unicode61'
);
CREATE TRIGGER articles_fts_ad AFTER DELETE ON articles BEGIN
    DELETE FROM articles_fts WHERE rowid = old.id;
END;
```

- 普通 FTS5 表：`rowid` = `articles.id`，无 `content=` 外置列。
- `porter unicode61`：英文词干化 + Unicode 分词（中文无空格词边界，按连续汉字组成 token；靠自动前缀命中部分匹配，见 §5）。
- 删除由触发器同步；**插入/更新不走触发器**，在代码事务内显式执行，避免读状态变更触发无谓重建。

### 写入/更新路径

| 时机 | 位置 | 动作 |
| --- | --- | --- |
| 文章入库（upsert 成功） | `db.rs` ~1471 | 同一事务内 `INSERT INTO articles_fts(rowid, title, body)`（`body` = 清洗后的 `body_text`） |
| 正文提取完成 | `db.rs` ~1962 | `UPDATE articles_fts SET body = ?2 WHERE rowid = ?1` |
| 文章删除 | 触发器 `articles_fts_ad` | 同步删 FTS 行 |
| 维护 | `db.rs` ~4178（admin/VACUUM 路径） | `INSERT INTO articles_fts(articles_fts) VALUES('rebuild')` 折叠 shadow 段（禁止直接删 `articles_fts_*`） |

## 3. 查询编译：用户文本 → 安全 MATCH（search.rs）

核心函数：`compile_search_with_dict(input, mode, dict) -> CompiledSearch`。

1. **tokenize**：手工字符扫描出 `Term`（可带 `title:`/`body:`/`feed:` 字段、`phrase`、显式 `*`）、`OR/AND/NOT`、括号；`-` 仅作词首一元 NOT。
2. **解析成 AST**：`And / Or / Not / Terms / Feed`（递归下降：OR → AND → 一元 → 主项）。
3. **编译回 MATCH 字符串**：
   - `feed:` 节点**不进入 MATCH**，单独收集为 `feed_prefixes`，由 SQL 对 `feeds.title` 做前缀过滤；
   - 每个裸词按非字母数字切分（对齐 unicode61，`rust-lang` → `rust` + `lang`），各部分自动加尾部 `*`（前缀匹配），**ASCII ≤3 的短词除外**（`AI` → `"AI"` 整词，避免命中 `against`）；显式 `chin*` 强制前缀；
   - 引号短语整串精确匹配 `"..."`（不自动加 `*`）；
   - 否定：FTS5 只有二元 `NOT`，所以裸 `-term` 只在 AND 语境下编译为 `A NOT B`；纯一元 NOT 丢弃/匹配空；
   - 括号 OR 后接词的场景显式补 `AND`（FTS5 不接受 `(a OR b) c` 的隐式 AND）。

### 模式（bare 词之间的默认连接）

| 模式 | 使用者 | 相邻裸词连接 |
| --- | --- | --- |
| `Strict` | Web/桌面列表搜索 | **AND**（越搜越窄） |
| `Recall` | RAG 检索、`papr search`（`--and` 转 Strict） | **OR**（召回优先） |

显式 `AND`/`OR` 在任何模式都优先于默认连接。

### 中英同义词展开（词云实体词典）

- 数据源：`wordcloud-entities.json` → `WordCloudDict`（`wordcloud_dict.rs`，按文件 mtime 缓存刷新，进程内 `Arc` 共享）。
- 只在**整词裸 token** 上做精确别名查找（不做子串匹配）；命中实体 → 编译为别名 OR 组（多词别名转精确短语），**不加自动 `*`**（短别名如 `ai` 不会变 `ai*`）。
- 同一实体在 AND/OR 内重复出现时**去重折叠**（`Trump 特朗普` 编译结果 == `Trump`）；`特朗普`/`Trump`/`川普` 都归 `person.trump`。
- 引号短语、`title:` 字段内同样可展开（仅限整 token）；短语本身不展开。
- 高亮术语 `highlight_terms(_with_dict)` 走同一 tokenize + 别名展开，供前端命中高亮。

## 4. 执行与排序（db.rs list_articles_sorted）

- `search` 参数编译为 MATCH 表达式，`feed:` 前缀进 SQL：
  ```
  FROM articles a
  JOIN feeds f ON f.id = a.feed_id
  JOIN articles_fts fts ON fts.rowid = a.id
  WHERE (kind/feed/folder/tag/unread 过滤) AND articles_fts MATCH ?
    AND f.title LIKE 'Reuters%'   -- feed: 前缀
  ```
- 排序（`article_order`）：
  - **搜索 + 相关度**：`fts.rank ASC`（FTS5 bm25，越小越相关）为主，日期为次；
  - 浏览（无搜索）：`datetime(COALESCE(published_at, fetched_at)) DESC/ASC`，由表达式索引 `idx_articles_sort` 支撑；`sort_by_relevance=false` 可强制搜索时按日期排。
- 分页 LIMIT/OFFSET；行集随后批量附加标签（`attach_article_tags`）。

## 5. 其它消费方

| 场景 | 入口 | 特点 |
| --- | --- | --- |
| 服务端列表 API | `routes/articles.rs`（kind/value + `search` + `sortByRelevance`） | Strict 模式 |
| RAG / AI 简报上下文 | `db::search_articles_for_rag`（`fts_query(question, true)`） | Recall 模式 + 词典；`fts.rank` 取 top-N；全停用词/纯标点 → 空结果 |
| CLI `papr search` | cli `search` | 默认 Recall；`--and` 转 Strict |
| 前端高亮 | `highlight_terms_with_dict` | 见 §3 |

## 6. 安全性

- 用户文本**从不直接拼接进 MATCH**：一律经 tokenize → AST → 编译器转义（引号翻倍）+ 白名单字段名。
- 解析失败（括号不匹配、残留 token、纯标点）→ `match_expr: None` / `"\"\""`（匹配空）。

## 相关文件

- `crates/papr-core/src/search.rs` — 词法/语法/编译/高亮/测试
- `crates/papr-core/src/db.rs` — FTS 表、写入同步、列表 SQL、bm25 排序、`fts_query`（RAG/旧调用）
- `crates/papr-core/src/wordcloud_dict.rs` — 实体词典加载与查找
- `crates/papr-server/src/routes/articles.rs` — HTTP 入口
- `docs/search.md` / `docs/search-synonyms.md` / `docs/user-search-and-tags.md`
