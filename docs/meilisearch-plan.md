# Meilisearch 接入规划与本地验证报告

> 状态：**本地验证完成，等待确认后进入 Rust 集成与线上实施**（2026-09-07）。
> 本地验证环境：Meilisearch 1.53.1（`127.0.0.1:7700`，样例 key）；语料 = 线上 2026-09-02 快照（30,130 篇 / 413 源 / body 均值 4.8KB，含真实标签）。

## 0. 已确认的决策

1. **引擎关系**：`settings.search_engine ∈ {meili, fts}`（默认 `meili`）；Meili 不可用时**自动回退 FTS5**，保留 FTS 路径与索引作为降级方案。
2. **索引范围扩展**：title + body + **feed 标题 + 作者 + 标签名（interest/ai）**；阅读状态等走 filter，不再进 SQL 过滤搜索路径。
3. **本地验证**用现跑实例（key `aSampleMasterKey`），验证后清理测试索引。

## 1. 为什么引入 Meili（相对 FTS5）

| 能力 | FTS5（现状） | Meilisearch |
| --- | --- | --- |
| 拼写容错 | 无 | 内置（`trumph` → Trump） |
| 相关性排序 | bm25 + 前缀硬规则 | 多因子（词频/typo/临近度/字段权重/exactness） |
| CJK 混合语料 | unicode61 无词边界，靠前缀 | 较好分词，中英日混合可搜 |
| 中英同义 | wordcloud dict 编译期展开 | 服务端 synonyms，双向生效 |
| filter/排序 | SQL 侧 | 原生 filter + 可排序字段 |
| 运维 | 内嵌零成本 | 独立进程 + 数据同步（主要成本） |

## 2. 目标架构

```
┌────────────────────────────  papr-server  ────────────────────────────┐
│  GET /api/articles?search=…                                            │
│      │ 现有 parser(search.rs) 把查询拆成 AST                             │
│      ├─ "简单查询"(裸词/短语, 无 NOT/括号/字段) ──► Meili client            │
│      │        （拼写容错 + synonyms + matchingStrategy + filter/sort）     │
│      ├─ "字段限定 title:/body: 单一字段"  ──► Meili attributesToSearchOn   │
│      └─ "复杂查询"(NOT/括号/混合字段) ──► 回退 FTS5（语义完全兼容旧契约）      │
│   故障/超时/索引空 ──► 自动回退 FTS5                                       │
└──────────────┬───────────────────────────────────────────────┬────────┘
       写入侧同步队列                                    read (HTTP 127.0.0.1:7700)
┌──────────────▼────────────────┐              ┌──────────────▼──────────────┐
│ meili_sync_queue(article_id)   │   worker    │ Meilisearch                 │
│ 文章增改/正文提取/标签变更入队     │ ─────────► │ index: papr_articles         │
│ 删除文章也入队(移除文档)          │  批处理     │ title/body/feed/author/tags  │
└────────────────────────────────┘              │ filter: feedId,isRead       │
      backfill: papr meili rebuild              │ sort: sortDate              │
                                                └─────────────────────────────┘
```

- **查询侧映射**（服务端复用 `search.rs` 词法/AST 判定，编译逻辑按引擎分流）：
  - 模式：UI/列表 = `matchingStrategy: all`（对应 strict AND）；RAG/CLI = 默认（对应 recall OR）。
  - `feed:` 前缀 → 照旧抽成 SQL `feeds.title LIKE`（无需 Meili 参与）；`feedId` filter 留给其它浏览查询。
  - `title:`/`body:` 单字段 → `attributesToSearchOn`；短语 → Meili 词序临近（近似，非强制短语）；命中数/分页用 `estimatedTotalHits` + offset。
- **filter 语义保持**：未读 = `isRead = 0`；阅读状态变化（标已读/星标）只需 `PATCH` 该文档 `isRead`（进同步队列），不再影响 FTS 正文查询。
- **排序**：搜索默认相关度（Meili ranking rules）；时间倒序浏览/搜索排序用 `sort: ["sortDate:desc"]`（`sortDate = epoch(COALESCE(published_at, fetched_at))`）。

## 3. 同步设计

- **写侧队列 `meili_sync_queue`**（新表，`article_id PK, kind: upsert|remove, updated_at`）：
  - 入队点：文章 upsert、正文提取完成、标签增删改（含合并/别名/层级变更涉及的批量文章）、`is_read`/星标等状态位变化、文章删除。
  - worker（papr-server 后台，与 auto-tag 同节奏）：每轮取一批（如 200），对 upsert 构建文档批量 `POST /documents`（任务式），remove 用 `DELETE /documents?ids=`；失败保留重试。
  - 文档构建与本地验证一致：`id/feedId/title/body(截断30k)/feed/author/tags[]/isRead/sortDate`。
- **首次/重灌**：CLI `papr meili rebuild --db …`（删除重建 + 分批上传 + 等待 tasks 完成）；作为部署步骤的一部分在切换前执行。
- **一致性**：周期性对账（Meili `numberOfDocuments` vs SQLite 计数，日志告警即可，不强一致）；队列积压指标可观测。
- **标签联动**：标签合并/删除影响多篇文章 → 受影响 `article_id` 全量入队重建（防碎片工具链已有受影响面）。

## 4. 配置项（DB settings，新键）

| key | 默认 | 说明 |
| --- | --- | --- |
| `search_engine` | `meili` | `meili` / `fts`；设为 fts = 一键回退 |
| `meili_url` | `http://127.0.0.1:7700` | 内网地址 |
| `meili_key` | — | 专用 key（最低权限：仅 papr 索引的 search/update） |
| `meili_index` | `papr_articles` | 索引名 |

## 5. 本地验证结果（30,130 篇真实快照语料）

| 用例 | 结果 | 说明 |
| --- | ---: | --- |
| CJK 标签词 `中东局势` | 2,203 命中，22ms | 前三命中均真实带该标签；中英日混合语料可搜 |
| CJK 简写 `伊朗` | 414 命中，9ms | 英文文章（含中文标签）也能搜到，精度高 |
| 严格 AND `tariff china`（all 策略） | 1,270 命中 | 顶部全部相关（关税/中美） |
| 拼写错误 `trumph` | 9,413 命中 | typo 容错正常 |
| 中文同义 `特朗普` → Trump 文章 | 9,529 命中 | synonyms 生效（含 `川普/trump/donald trump`） |
| 短语 `federal reserve` | 1,657 命中 | 邻近度排序，顶部高度相关 |
| title 字段限定 | 正常 | `attributesToSearchOn` 可用 |
| 未读过滤 `isRead = 0` | 正常 | filter 生效 |
| 未读 + 时间倒序 | 正常 | sortDate 生效 |
| 无结果词 | 0 | 正确 |
| 延迟（20 条真实查询） | **p50 5ms / p95 11ms** | localhost；服务器端预计 <50ms 量级 |

**注意点**：索引磁盘占用较大（30k 文档 ~1.8GB，raw 257MB）——主要来自完整正文倒排，规划服务器数据盘 5–10GB；语料与线上同构，规模结论可平移。CJK 分词为 Meili 默认（日语/中文连续字串处理良好），如需更精细可后续用 `separatorTokens`/自定义词典微调，不影响接入。

## 6. 行为差异（与 docs/search.md 契约）与回退

- Meili 无布尔语法：`NOT`/括号/显式 `AND`/`OR` 的复合查询走 **FTS 回退**（语义与旧版一致）。前端现有入口基本为自由文本，极少触发。
- 引号短语在 Meili 为词序临近而非强制相邻（近似）；纯短语精确性要求高的场景回退 FTS 即可——v1 实现按"含引号短语 → FTS"简化（短语查询少、且 FTS 保真），后续可用 Meili 1.10+ 的 phrase search 升级。
- 命中总数：Meili 为估算（`estimatedTotalHits`），分页语义一致，数字与 FTS 精确计数会略有出入。
- 相关度排序与 bm25 不同（预期内，通常更好）；会带来列表默认顺序变化，属可接受的产品变化。

## 7. 线上实施步骤（确认后执行，分两个发布）

**阶段 1 — Rust 集成 + 开关（v0.1.18）**
1. `crates/papr-core`：`meili.rs`（HTTP client：search/add/delete/settings/health，使用 reqwest，与 ai.rs 相同）；settings 读取；`meili_sync_queue` 迁移 v33 + 入队钩子（文章写路径、标签变更、状态位）；`search_engine` 分流逻辑（复用 search.rs AST 判定简单/复杂）。
2. `papr-server`：搜索路由接 Meili（异步 HTTP）+ 自动回退；后台 worker 消费同步队列。
3. `papr-cli`：`papr meili rebuild`、`papr meili status`。
4. 测试：core 单测（编译分流、队列、文档映射）+ 本地用测试实例端到端验证（同一实例的 `papr_articles_test` 可复用）；本地全部通过后打 tag v0.1.18。
5. **部署 v0.1.18，但 `search_engine` 保持 `fts`** —— 仅上线"索引同步"，观察若干天（队列追平、无报错）。

**阶段 2 — 切换 Meili（确认后 0 风险切换）**
6. 服务器部署 Meilisearch（systemd，`127.0.0.1:7700`，master key 入 deploy.env，数据目录独立）。
7. 首次 `papr meili rebuild` 全量回填 + 对账（30k 文档数一致）。
8. 设置 `meili_url/key/index`，`search_engine = meili`；抽样查询对比 FTS 结果。
9. 观察：日志回退次数、同步队列深度、查询延迟；异常即刻 `search_engine=fts` 回退。
10. 回滚预案：改回 `fts`（数据无需清理，FTS 索引仍在）。

## 8. 风险与缓解

| 风险 | 缓解 |
| --- | --- |
| Meili 进程/磁盘故障 → 搜索不可用 | 自动回退 FTS；systemd 自愈；队列持久化不丢同步 |
| 索引与库短期不一致（同步延迟） | 秒级 worker；对账告警；接受最终一致 |
| 磁盘占用 | 数据盘 5–10GB 预算；必要时 body 截断/压缩（`compress` 实验） |
| 查询语义差异 | 复杂查询保 FTS；短语/字段查询可配；发布前对比测试 |
| 生产 master key 泄露面 | 只绑定内网；Meili 专用 search/update key，禁 admin key 外泄 |

## 9. 本地环境清理（验证后）

- 删除测试索引 `papr_articles_test`（可选保留供阶段 1 复用，重建 <2 分钟）。
- 临时文档分块 `/tmp/papr_meili/*` 与脚本已生成，验证脚本留存（`/tmp/papr_meili_export.py` 等，不入库）。

## 10. 补充：Hybrid（关键词 + 向量语义）本地验证

> 结论：**语义混合检索在 BGE-M3 下验证通过**，多语言/意译类查询显著优于纯关键词；可作为 Meili 接入的增强选项（默认 semanticRatio 0.5 附近，可按体验调）。

### 验证设置

- 本地 Meilisearch（重启后换全新数据目录 `/tmp/papr-meili-data`，`--master-key=aSampleMasterKey`），BGE-M3 经本机 ollama（`localhost:11434/api/embed`，1024 维）。
- 索引 `papr_articles_hybrid`：2,000 篇真实快照子集，`_vectors` 用 `userProvided` embedder 注入（**嵌入文本 = 标题+源+作者+标签+正文前 600 字**——全量正文嵌入 CPU 太慢且非必要）。
- 嵌入吞吐：批量 96/次 ≈ 2000 篇 1 分钟内完成（CPU）。索引体积：2k 篇 ≈ 213MB（向量为增量大头，30k 篇估 ~3GB，规划数据盘应上调到 10GB 级）。

### 结果（要点）

| 查询 | 模式 | 效果 |
| --- | --- | --- |
| `伊朗` | 关键词 | 命中精确（含中文标签的文章） |
| `特朗普`（无同义词词典） | hybrid 0.5 | 命中含西语 Trump 文章——向量跨语言桥接 |
| `中国经济放缓担忧`（正文全是英文） | 关键词 | 基本无效（无字面命中） |
| 同上 | hybrid 0.6 | 命中 Walmart 增长疲弱/Slovak 竞争力下降等**语义相关**英文报道 |
| `中东紧张局势推高原油价格`（中文查英文） | hybrid 0.5 | 顶部全部命中 Oil price/Middle East tension 类英文文章（强） |
| `关税 供应链冲击` | vector-only 1.0 | 命中关税/贸易战相关英文文章 |
| 欧央行加息（英查多语） | hybrid 0.6 | 命中多国央行加息文章 |
| 延迟 | — | 2–17ms（localhost，2k 文档） |

- `estimatedTotalHits` 在混合模式会回到全库规模（语义分不设硬过滤），只作分页用。
- 生产中查询需**先对查询词做一次嵌入**再带 `vector` 调 Meili（userProvided 模式），或改用 Meili 托管 embedder 让 Meili 自行嵌入查询词。

### 踩坑记录（影响生产选型）

1. Meili 内建 `ollama` embedder：settings 校验要求 URL 以 `/api/embed` 结尾，但运行期实际请求报 `bad uri: Rejected URI` 并无限重试（1.53.1），**不可用**。
2. Meili 内建 `rest` embedder：response 指针解析 ollama 响应结构失败（`"{{embedding}}" not found`），文档站为 JS 渲染无法快速核验 schema；需以 OpenAI 兼容结构（`{"data":[{"embedding":[...]}]}`）实测。
3. 因此本地验证走 `userProvided`（向量自产自传），绕开托管 embedder 的坑。
4. embedder 变更会触发**对存量文档补嵌入**（阻塞式、慢），配置前先清空/规划好再填。
5. 调度器可被一个卡死的 embed 任务整体堵住（cancel 无效，需重启实例）；多任务队列在重启后恢复执行。

### 对生产架构的影响（混合模式）

- 需要一个稳定的嵌入服务：优先 **OpenAI 兼容 REST embedder**（BGE-M3 自己起一个 OpenAI 兼容端点，或在 Meili 侧用 `rest` 源并配 OpenAI 形状的 `request/response`），并在阶段 1 集成时先做 1 篇实测再全量。
- 查询路径：papr-server 负责把查询文本交给嵌入服务取向量（userProvided 时）或配置 Meili 托管 embedder（rest/ollama 修好后）。
- 索引字段模板应只嵌"标题+标签+摘要片段"，控制成本与体积。
- `semanticRatio` 做成配置项（0=纯关键词，1=纯向量，默认 0.5），故障时回退关键词/FTS。
