# 标签（打标）实现总结

> 面向实现者的架构说明：AI 自动打标怎么触发、怎么让模型输出、怎么解析、怎么落库，以及防碎片/合并/层级的治理机制。2026-09 状态（v0.1.17 之后）。

## 1. 总体：双词表设计

所有标签都在一张 `tags` 表，用 `kind` 区分两套词表（`models.rs`：`TAG_KIND_AI` / `TAG_KIND_INTEREST`）：

| kind | 语义 | 规模（线上） | 管理方式 |
| --- | --- | --- | --- |
| `interest` | 用户定义的**封闭兴趣词表** | 14 | 手工增删；模型只许"选"，不许造 |
| `ai` | **自由生成**的打标词表 | ~32,772（清理 876 零使用后） | 模型可造新词，但写侧防碎片 + 定期治理 |

标签结构（迁移 v30/v32/v34）：`tags(name, color, position, kind, created_at, reviewed_at, parent_id, tag_type)`、`tag_aliases(kind, alias → tag_id)`（旧拼写钉定，防复发）、`tag_suppressions(tag_id, count)`（被用户否定的标签，自动打标跳过）、`tags.parent_id`（**单父、最多两层**）+ `tags.tag_type`（`entity`/`topic`）。

## 2. 触发与队列

### 入队时机（`db.rs` upsert 文章后）

- 只要 `auto_tag_enabled` 或 `ai_tag_enabled` 任一开启，新入库文章即写入 `auto_tag_queue(article_id, status, attempts, last_error, …)`（`ON CONFLICT` 重置为 pending）。
- **陈旧保护** `ai_tag_max_age_days`（默认 3，0=关闭）：入库时解析 `published_at`，超过 N 天的**不回填打标**（历史上新订阅整站历史是 token 尖峰来源）；无日期按新文章处理。
- 首次抓取深度由 `feed_initial_backfill`（默认 50）封顶，进一步限制历史回填。

### 消费（worker）

- `papr-server` 后台任务每轮 `claim` 队列：新入库优先于回填积压（`claim_new_ingest_cuts_ahead…`），偏好较新发布时间；`attempts ≥ MAX_ATTEMPTS(3)` 停退；status：`pending → active → done`，失败记录 `last_error`。
- 支持 API：状态/回填/清队（`/api/auto-tag/*`）、单篇 `POST /api/articles/{id}/auto-tag`。
- **每日预算** `ai_tag_daily_budget`：worker 依据 `ai_usage` 当日累计限制调用量，防止内容尖峰意外计费。

## 3. 单篇打标流程（`auto_tag.rs`）

### 3.1 上下文加载 `load_job_context`

| 项 | 来源 | 说明 |
| --- | --- | --- |
| 开/关 | `auto_tag_enabled`、`ai_tag_enabled` | 都关 → 报错跳过 |
| interest 词表 | `list_tags(interest)` | 全量（封闭小词表） |
| AI 复用词表 | `top_tag_names(ai, ai_tag_prompt_cap)` | **按使用数排前 150**（默认 150，0=不给列表）；命中即复用、防同义分裂 |
| 每篇上限 | `auto_tag_max_tags_per_article` / `ai_tag_max_tags_per_article`（默认 5） | 打标前先算已挂数，`at_tag_caps` 满则**跳过 LLM** |
| 输入文本 | `title` + `summary`（无则 body 前 800 字） | 绝不把完整 HTML 正文喂给模型 |

### 3.2 提示词三分支 → 模型输出 @tags

（v0.1.17 起输出格式从 JSON 改为 **单行 @ 标签**，P0/P1 重构）

| 分支 | 条件 | 指令要点 |
| --- | --- | --- |
| `prompt_combined` | interest+ai 都开 | 一条 @ 列表混排：interest 命中要 **verbatim 精确拼写**（闭集路由用）；AI 部分 REUSE 前 150 词表；真新闻 ≥2、总 ≤8 条 |
| `prompt_interest` | 仅 interest | 只许从列表现有词里选（≤5），没有可匹配就回 `@none` |
| `prompt_ai` | 仅 ai | 主题性短标签 2–5 个；**格式规则（强制）**：全小写、多词 kebab-case（`@middle-east`）、自然复数化、缩写展开（`@ai→@artificial-intelligence`）、专名/中文保留；真新闻不许空 |

调用：`ai::complete_chat_classify` —— 不带 JSON `response_format`，但 **DeepSeek thinking 仍显式关闭**（`disable_thinking` 从 json_object 块解耦成独立参数；分类任务思考只会烧掉 512 token 预算）。AI 配置（provider/api key/model/base_url）从 DB `settings` 读。

### 3.3 解析：正则抽取（不再 JSON repair）

1. `preprocess_model_text`：剥 `<think>`/fence 等包装。
2. 正则 `@([\p{L}\p{N}][\p{L}\p{N}-]*)` 抽 token → 小写化 → `normalize_tag_name`（trim、≤32 字符、需含字母数字）→ 去重保序。
3. 语义三态：
   - 有标签 → `Ok(tags)`；
   - 一个 token 都没有 → `SoftEmpty`：文章**有内容**则触发**一次 repair**（追问"只回 @ 标签"），仍空则硬失败记 attempts；无内容（fluff）直接软跳过；
   - `@none` 单独/混合 → **成功空**（对应旧 JSON 的 `{"tags":[]}`，真新闻无匹配时不多烧一次 repair）。

### 3.4 路由与落库

- **combined 扁平列表** → `route_tag_buckets`：每个 token 经 `resolve_interest_name` 判定（精确名 → 别名 → surface 变体 `surface_alnum_key`，如 `@middle-east` → 词表里的 `Middle East`），命中则进 interest 桶（规范化名、去重），否则进 AI 桶。
- **interest-only** → 同样过 `route_tag_buckets`，未知 token 丢弃（模型被告知不许造）。
- **ai-only** → 直接走 AI 写路径。
- `apply_suggested_tags`（interest）：只挂能解析到现有 interest 标签的（内存精确/别名/表面变体兜底），逐篇 cap 内。
- `apply_ai_tags`（ai）：每个名字先 `resolve_ai_tag_for_writing` —— (1) 精确/别名；(2) 对整库 AI 标签做 case/标点/空格不敏感的表面变体扫描；**真没有才 `create_tag`**（幂等）。这就是写侧防碎片的核心（v0.1.17 起）。
- 每类各按自己 cap 截断；空结果打 debug 日志。

## 4. 防碎片与治理（标签过多的问题及对策）

### 现状成因

AI 自由打标 + 模型每篇微调措辞 → 词表长尾：线上曾 33,810 AI 标签、**约 2/3** 只挂过一篇文章、同主题分裂出几十个近义拼写（"中东"相关 46 个变体）。一次命中是**自然长尾**（单次事件/人名），无害也不进复用列表；真正的垃圾是**零使用标签**（已清理 876 个）。

### 治理机制

| 层 | 机制 | 位置 |
| --- | --- | --- |
| 写侧 | 别名 + 表面变体复用，杜绝复发 | `auto_tag.rs::apply_ai_tags` |
| 反馈抑制 | 阅读中**移除 AI 标签 = 一次否定**：写侧跳过该标签（复用列表也剔除），管理员可恢复；merge 时否定记录随词合并到保留词 | `routes/tags.rs`（detach 落 `tag_suppressions`）、`db.rs`、`auto_tag.rs::apply_ai_tags` |
| 待审队列 | 30 天内新建 / 仅 1 篇 / 未挂父级 entity 的 AI 标签进 **review 队列**，可确认（`reviewed_at`）、屏蔽、删除、挂父级/设类型 | `routes/tags.rs`（`/api/tags/review-queue`、`/review`、`/suppress`、`/hierarchy`）、`db.rs::review_queue` |
| 确定性合并 | surface fold：case/标点/空格/连字符不敏感归并（`Middle East`≡`middle-east`），全词表零 LLM 成本 | `tag_taxonomy.rs::deterministic_groups` |
| LLM 语义聚类 | 仅对 **≥min_count(3)** 的长尾分批（上限 800，60/批）问模型分组/层级，坏批跳过；`ai_usage` 记 `tag-tidy` | `tag_taxonomy.rs::build_plan` |
| 落地 | `apply_plan`：`merge_tags_keep_alias`（旧拼写写 `tag_aliases`）+ 设 `parent_id`/`tag_type`；plan 可存 JSON、审阅、编辑、`papr tag apply plan.json` 重放（已验证本地/线上 1:1） | CLI `papr tag tidy` / `papr tag apply` |
| 线上手工 | 单对合并 + 别名 CRUD 的 HTTP 接口 | `routes/tags.rs` |

### 层级（v0.1.17 上线内容）

- 语义：`entity`（人/地/机构）挂到 `topic`（区域/主题）下，如 `伊朗(entity) → Middle East`、`Kenya → Africa`、`ChatGPT → artificial intelligence`。
- 约束：**单父、两层深**（无孙级），所以"中东 → 伊朗 → 伊朗危机"放不下；事件类 topic 与实体分开挂。
- 已应用：合并 179 标签、重指 700 关联、179 别名、402 层级/类型行（2026-09）。
- UI/API：`GET /api/tags` 的每个 tag 现携带 `parentId` / `tagType`；侧栏 Tags tab 按 **topic → entity 树**展示（父级可展开/收起，状态持久化）；**点击父级 = 列出该标签及其子级全部文章**（`ArticleQuery::Tag` 的 WHERE 含直接子级：`tag_id = ? OR tag_id IN (SELECT id FROM tags WHERE parent_id = ?)`，列表与 mark-all-read 共用同一语义）；review tab 可改类型/父级，`POST /api/tags/{id}/hierarchy` 做合法性校验（见 §4）。

### UI 侧（2026-09）

AI 标签列表新增**最小使用数筛选**（`全部 / ≥1 / ≥5 / ≥10 / ≥20`，localStorage 持久化），避免长尾噪音淹没浏览；默认仍为"全部"。

标签管理（设置 → 标签管理）新增两个 tab：
- **待确认（review）**：默认近 30 天新建未过目的 AI 标签，可按 `新标签 / 仅 1 篇 / 未挂父级实体 / 全部` 切换；每行给出使用数与最近 2 条文章标题便于判断，动作 = 确认（`reviewed_at`，离开队列）/ 屏蔽 / 删除 / 改类型（entity/topic）/ 挂父级（输入已存在的 topic 名，留空回到顶层；entity 不能作父、两层深、已有子级的标签不能下挂——`db::validate_tag_parent_link`）。
- **已屏蔽（suppressed）**：被否定的标签列表（含否定次数），一键恢复后写侧重新允许自动附上。

侧栏 **Tags tab**：AI 标签按层级**树状展示**——带子级的 topic 顶层显示（默认展开，箭头可收起，`papr.aiTagTreeCollapsed` 持久化），其下缩进列出 entity/子标签；点击父级列出**整棵子树**的文章（列表 + 标记已读同语义），点击子级只看自身。当前选中的子标签若父级被收起会自动展开（同 folder reveal 逻辑）。子级行用小圆点弱化视觉，父级保留彩色 dot。

阅读器里移除 AI 标签会即时提示"已记住，不再自动打"，可在上列 tab 恢复。

## 5. settings 速查（DB `settings` 表）

| key | 默认 | 作用 |
| --- | --- | --- |
| `auto_tag_enabled` / `ai_tag_enabled` | 0 | 两套词表的总开关（任一开则入队） |
| `auto_tag_max_tags_per_article` / `ai_tag_max_tags_per_article` | 5 | 每篇每种最多挂几个 |
| `ai_tag_prompt_cap` | 150 | 喂给模型的 AI 复用词表大小（0=不给列表） |
| `ai_tag_max_age_days` | 3 | 入库打标的历史陈旧门槛（回填保护） |
| `feed_initial_backfill` | 50 | 新订阅首次抓取深度上限 |
| `ai_tag_daily_budget` | — | worker 每日调用预算上限 |
| `ai_provider` / `ai_model` / `ai_api_key` / `ai_base_url` | — | LLM 配置（deepseek / deepseek-v4-flash） |

## 相关文件

- `crates/papr-core/src/auto_tag.rs` — 提示词、@ 解析、路由、落库、队列单篇处理
- `crates/papr-core/src/ai.rs` — `complete_chat_classify`（thinking 解耦）
- `crates/papr-core/src/tag_taxonomy.rs` — tidy 计划/层级/合并落地
- `crates/papr-core/src/db.rs` — 队列、settings、aliases、merge/alias API
- `crates/papr-server/src/routes/auto_tag.rs` / `tags.rs`、`crates/papr-server/src/jobs.rs` — HTTP 与后台 worker
- `crates/papr-cli/src/main.rs` — `papr tag tidy` / `tag apply`
- 前端 `src/components/Sidebar.tsx` — AI 标签列表（最小使用数筛选）
