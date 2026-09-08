# Papr 线上更新计划 v0.1.24 → v0.1.25

> 制定：2026-09（本地收尾后）
> 状态：**计划待执行**——所有动作均未发生；上线前需你 ① 在 demo（http://127.0.0.1:7411，admin/papr2026）实机确认 UI ② 对本计划逐条点头。
> 原则：先备份、可回滚、分步验证；迁移是追加式（v34+v35），不回改历史。

## 1. 现状快照

| 项 | 本地 | 线上（root@8.130.99.81） |
|---|---|---|
| 代码 | main = v0.1.24(bf5febc) + 2 本地提交(b274f9c 只读连接、e795ce9 预热/分块) + **大量未提交改动** | v0.1.24（= bf5febc） |
| DB 版本 | demo user_version 35 | user_version **33**（v34/v35 均未上） |
| 搜索 | meili 1.6（本地 /doc/meili） | meili 1.6.0 systemd，index `papr_articles`（~35k docs） |
| 打标引擎 | demo 关闭（auto/ai_tag off） | 按现状，不动开关 |

线上待升级库会依次跑 **v34**（反馈闭环：`tags.created_at/reviewed_at`、`tag_suppressions` + tags 级联；meili 同步队列/触发器）与 **v35**（`tags.domain` 可空列）——均为追加式迁移。

## 2. 本次上线内容清单（用户可感知）

- **标签反馈闭环（v34）**：移除 AI 标签会被记录/屏蔽；打标跳过屏蔽词；tag tidy/apply 合并时转移否定记录；治理 API（review-queue / suppress / restore / hierarchy）。
- **领域机制（v35）**：`tags.domain` 字段 + 校验 + `GET /api/tags/{id}` + hierarchy 支持 domain。**上线后所有标签 domain = NULL**（按架构评审：不做数据回填，机制保留）。
- **管理入口整合**：设置 →「标签管理」大面板删除，改为「AI 打标」引擎页（开关/上限/队列/回填）；标签管理统一到侧栏「标签」→「管理」全屏管理台（标签浏览｜待确认｜已屏蔽 三视图）。
- **搜索/读写性能（本地 2 提交）**：列表/搜索走独立只读 SQLite 连接；wordcloud 回填分块让锁；meili 预热更丰富。
- **不包含**（后续迭代，不进本次）：新词人工 gate、C1 自动降级清理、词库单一事实源、Meili 升级/Hybrid、领域补猜与消费 UI。

### 2.1 标签数据与代码变更的关系（重要：本次升级**不搬任何标签数据**）

线上数据在持续更新（新文章、重打标），而本地治理是在快照/演示库上做的——两者必须分清：

**随发布走的（代码与结构，自动、无冲突）**
- v34/v35 迁移只**加列/建表**：`created_at/reviewed_at` 与 `domain` 对线上存量标签一律置 **NULL**，`tag_suppressions` 从空表开始。已核对实现：review 队列 `new` 过滤器要求 `created_at IS NOT NULL 且 30 天内`，存量标签（NULL）**不会**涌入待确认队列。
- 治理/屏蔽/管理台/引擎页是**行为代码**：升级后作用于"之后发生的事"（新词、用户移除标签、管理员操作），与线上已有数据漂移与否无关。

**不随发布走的（本地演示库的数据操作，不推送、不套用）**
- 本地做过的 tidy 合并/别名/一次性词清理/层级与 domain 测试值，都只存在于 `backups/demo/papr.db`，属于快照上的试验，**线上绝不直接套用**——线上标签计数、文章样本已变化，快照期决策会过时，且本地 suppression 记录不代表线上用户的真实反馈。

**线上标签治理的正确姿势（升级之后做，非本次发布动作）**
1. 升级稳定后，用**线上实时数据**跑 `papr tag tidy`（干跑，只出 plan JSON，不改库）——新代码已含 v34 语义（created/reviewed 判定、suppression 转移）。
2. 本地历史整理（哪些词该合并/清理、层级怎么挂）作为**参考候选清单**喂给人工审核，但计划必须基于线上现数据重新生成。
3. 计划单交你审（对照审核队列/样本），确认后 `papr tag apply`——**永远不在线上自动应用治理**。
4. 若线上 AI 打标在跑：升级后新词自动获得 `created_at`（进 review 队列 `new`）、用户移除自动计 suppression——反馈闭环从零自然累积，观察即可。

## 3. 提交与版本策略（发布前，需你确认）

把当前工作树收拢为**干净提交序列**（本地整理，不 push 前给你过目）：

1. `feat(tags): v34 feedback loop — suppressions/review queue, governance endpoints & CLI`
   （papr-core db v34、tag_suppressions、taxonomy carry_suppression、auto_tag 跳过屏蔽、routes、cli、相关测试）
2. `feat(tags): v35 domain layer — tags.domain, validation, get-one & hierarchy domain`
   （papr-core v35 + models TAG_DOMAINS、routes tags、tests）
3. `refactor(ui): tag management workbench — unify entry (Option A), review/suppressed views, engine-only settings`
   （TagManageDialog、SettingsDialog、Sidebar、api/types、i18n、styles）
4. `perf(server): read-only conn for list/search + chunked backfill/warm-up`（= 现有 b274f9c、e795ce9 已在提交里，无需重做；只需确认顺序在 UI 之前/之后均可）
5. `docs: roadmap architecture verdict + production update plan`

随后：`git push origin main`（main 推送不触发发布）→ 打 tag `v0.1.25` 并 push → GitHub Actions `release-server.yml` 构建 musl 产物，产出 `papr-linux-amd64-v0.1.25.tar.gz`（bin/papr-server、bin/papr、dist/、.env.example、papr-server.service）。版本号沿用 tag（Cargo/package.json 维持 0.15.0，与 v0.1.24 同一做法，不额外 bump）。

**发布前本地检查**（我会执行，产物给你）：
- `cargo test -p papr-core`（v35 后重跑，363+ 全绿）与 `cargo check -p papr-cli`、`cargo check -p papr-server`
- `tsc --noEmit`、`pnpm build`
- **用线上真实库副本做迁移演练**：从服务器拉一份 `papr.db`（sqlite `.backup`），本地用新二进制 `PAPR_DB=<副本>` 启动到 v35，验证 health、user_version=35、review-queue/suppressed 空返回、搜索路径正常——**确认 v33→v35 一次成型**，演练库不回流。

> ⚠️ 每次 tag 构建历史上可能卡 runner（easyspace-ai 账号无管理员、无法取消）：发布当天留出观察窗口；卡住时先在 GitHub 页确认 job 是否仍在推进，别重复打 tag。

## 4. 线上执行步骤（你确认后按序做，每步留日志）

### 0. 前置
- 你已实机确认 demo UI；本地 5 个提交 + tag v0.1.25 已推送；GH Actions release 完成，拿到 asset URL。

### 1. 服务器备份（不可跳过）
```bash
ssh root@8.130.99.81
cd /product/papr
# DB 一致备份：先 WAL checkpoint 再 copy
sqlite3 data/papr.db "PRAGMA wal_checkpoint(TRUNCATE);"
tar -czf /root/backups/papr-v0124-$(date +%Y%m%d-%H%M).tar.gz \
  data/ bin/ dist/ .env 2>/dev/null || tar -czf /root/backups/papr-v0124-$(date +%Y%m%d-%H%M).tar.gz data/ bin/ dist/
# 记录当前状态
sqlite3 data/papr.db "PRAGMA user_version;"
curl -s localhost:PORT/api/health   # 按实际端口
```

### 2. 拉取产物并停服
```bash
cd /tmp && curl -L -o papr-v0.1.25.tar.gz <release-asset-url>
tar -tzf papr-v0.1.25.tar.gz | head
systemctl stop papr-server
# stop 后再做一次 DB 快照（一致点）
cp data/papr.db /root/backups/papr.db.pre-v0125.sqlite
```

### 3. 安装新版本
```bash
mv /product/papr/bin/papr-server /product/papr/bin/papr-server.old-v0124
tar -xzf /tmp/papr-v0.1.25.tar.gz -C /product/papr --strip-components=0  # 按实际包内布局
# 或手动 install bin/papr-server、bin/papr + rsync -a dist/（以解包后的 layout 为准）
chmod +x /product/papr/bin/papr-server /product/papr/bin/papr
ls -l /product/papr/bin/
```

### 4. 启动 + 观察
```bash
systemctl start papr-server
journalctl -u papr-server -n 100 --no-pager    # 看迁移日志（v33→v34→v35）有无报错
sleep 10 && curl -s localhost:PORT/api/health
```

### 5. 上线验证清单（逐条打勾）
- [ ] `/api/health` ok；网页能登录（强刷 Ctrl/Cmd+Shift+R 取新静态资源）
- [ ] `sqlite3 data/papr.db "PRAGMA user_version;"` == **35**
- [ ] `tags.domain` 列存在、全 NULL；`tag_suppressions` 表存在且空
- [ ] 存量标签 `created_at/reviewed_at` 全 NULL（`SELECT count(*) FROM tags WHERE created_at IS NOT NULL;` == 0）——**review 队列 `new` 视图为空**（不会涌入 1 万存量词）
- [ ] `GET /api/tags/{id}`（任选一 tag）返回含 `"domain":null`；review-queue（默认 new）与 suppressed 端点 200 且空列表
- [ ] 搜索仍走 meili：实际 query 响应正常（可在环境开 PAPR_DEBUG_TIMING 对比，验完即关）
- [ ] meili index 无卡死任务；worker 预热日志出现（每分钟 warm pass）
- [ ] UI：设置 →「AI 打标」引擎页；侧栏标签 →「管理」三视图可切换、待确认空态正常
- [ ] 打标引擎开关保持原状；若线上有队列在跑，观察 15 分钟消费无 error 激增

### 5a. 搜索引擎全量重建（`papr meili rebuild`，验证清单通过后执行）

**为什么要重建**：v0.1.25 的文档结构本身无变化，且改名/合并/标签增删**已实现自动同步**（`rename_tag`/`merge_tags` 都会把受影响文章入队，worker 自动重建对应文档，无需每次治理后手动跑）。本次重建的意义是**基线一致性**：线上从 v0.1.24 至今一直在增量写入，发布前若同步队列曾积压/中断，或存在任何未覆盖的漂移（标题/正文/标签名），一次全量重建让索引 = 线上 DB 现状，给升级与后续治理一个干净起点。命令幂等，按 meilisearch-plan §3/§7 规范执行。

```bash
# 在服务器 /product/papr 下，用线上 DB 与配置
./bin/papr meili rebuild --yes --db data/papr.db
# 或（若 CLI 默认路径约定一致）./bin/papr --db data/papr.db meili rebuild --yes
# 过程：ensure_index(重打 settings) → delete all docs → 按 id 分页 500/批上传
# ~35k 文档，正常几分钟；每批打印 "indexed N articles (through id M)"
```

**执行注意**
- 重建窗口内搜索短暂不完整（先清空后回填）→ 选低峰执行，个人服务器可接受；重跑即重来，无需清理残留。
- **meili 1.6 任务偶发卡死**：重建发 delete+几十个 upsert 任务，若进度卡住 → `systemctl restart meili` 后**重跑同一命令**（幂等）。
- 重建完成后做**对账**：`papr meili status`（或 /indexes/papr_articles 的 numberOfDocuments）≈ 线上文章数；抽样几条近期文章搜得到、tags 命中正确。

### 6. 回滚预案（任一验证失败）
```bash
systemctl stop papr-server
# 还原 DB（迁移不可向下兼容旧 binary：v35 库配 v0.1.24 会因版本超前启动失败，必须整库还原）
cp /root/backups/papr.db.pre-v0125.sqlite /product/papr/data/papr.db
mv /product/papr/bin/papr-server.old-v0124 /product/papr/bin/papr-server
# dist 如被覆盖：从步骤 1 的 tar 还原 dist/
systemctl start papr-server
# 验证回到 v33 + health ok，再决定排查
```

## 5. 风险与注意事项

- **迁移只加不回改**：v34/v35 都是新增表/列 + 触发器，生产 v33 直上 v35 是本次计划的核心假设，由步骤 3（本地副本演练）先证一次。
- **meili 影响面小**：domain 不在 searchable fields；改名/合并/标签增删已自动入队同步（`rename_tag`/`merge_tags` 调用 `enqueue_search_index_for_tag`），常规治理后**无需手动重建**——可在上线后小批验证一次（rename 一个词 → 搜旧名确认不再命中 tags）。
- **GH Actions**：只 push tag 触发 `release-server.yml`；`ci.yml` 前端 job 历史红与本次发布无关。
- **meili 1.6 偶发任务卡死**：不动 meili；如验证中发现 tasks stuck 再单独重启 meili systemd。
- **demo 遗留**：demo 库里有 1 个测试 domain（tag「AI」=Economy），属本地数据，不影响线上（线上全部 NULL）；如需清理可下次重建 demo。
- **上线后的治理**（合并/清理/层级）一律走 §2.1：先 `papr tag tidy` 干跑出**基于线上现数据**的 plan JSON → 你审 → apply。本地历史清单只作参考输入，绝不在线上自动套用快照期决策。
- **手动 rebuild 的兜底场景**（非治理必需）：升级时基线重建 §5a、队列积压/中断后、对账（numberOfDocuments vs 文章数）不一致、或索引数据异常——`papr meili rebuild` 幂等可随时重跑。

## 6. 本次收尾状态

- [x] 本地：v34+v35 代码、管理台整合、性能提交就绪（cargo check/tsc/pnpm build 均绿，demo v35 端到端验证过）
- [x] 文档：refactor-roadmap.md 已记录架构评审裁决与顺序重排；本计划成文（含 §5a 搜索引擎全量重建）
- [ ] 待你：实机确认 demo UI → 确认本计划 → 我再做 §3 提交收拢 + 本地迁移演练 → 出 release 候选 → 按 §4（含 §5a 重建）执行
