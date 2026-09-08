# AI 词表自动分层整理（2026-09-08）

> 方法：`papr tag tidy --auto-parents`（本次新增能力）全自动跑一遍 AI 词表。
> 目标库：生产快照 `papr-prod-snap-20260908.db.gz`（33,324 个 AI 标签）的**副本**，
> 原文件/线上均未改动。产物可直接 `papr tag apply` 到副本或线上，先审计划再审阅本报告。

## 0. 一句话结论

把 AI 词表从「99 个子级」铺到「1,014 个子级」，自动补建 1 级父主题；三层/entity 作父
等结构性红线零新增（本库原有 5 条三层链与 5 个 entity 父级系第一轮遗留，未动）。

## 1. 命令与参数

```bash
papr --db <work.db> tag tidy --kind ai \
  --min-count 10 --max-tags 2000 --batch-size 60 --auto-parents \
  --plan-out plan-auto-20260908.json        # 只出计划（不改库）
papr --db <work.db> tag apply plan-auto-20260908.json --yes   # 审完再落地
```

- 候选范围：**≥10 篇文章**的顶层叶子标签，1,676 个（结构性标签——已有父级/已有子级——
  一律不动、不进模型）。
- LLM：deepseek-v4-flash，28~31 批 × 60 标签，每批带 top-3 标题消歧；计费记账到
  `ai_usage`（feature=`tag-tidy`），只发生在副本库。
- 模型可把子级挂到**词表内任意已有顶层 topic**（候选池 ≤600），或提案**新 1 级主题**
  （`--auto-parents`）；提案经「已存在即复用 / 同拼写折叠 / 单子级新父丢弃」三步规整后，
  才进入计划 `parentsToCreate` 由 apply 创建。

## 2. 结果

| 指标 | 之前 | 之后 |
|---|---:|---:|
| AI 标签总数 | 33,324 | 33,292（合并 33 + 新建父 1） |
| 有父级（2 级） | 99 | 1,014 |
| topic 类型 | 434 | 1,139 |
| entity 类型 | 135 | 418 |
| 自动新建 1 级父主题 | — | 1（`US state news`：Virginia/Arizona/Pennsylvania/Nevada…） |

- 合并 32 组（33 个重复/变体标签），1,108 篇文章重挂；旧拼写钉为别名防复发。
- 层级/类型分配 1,024 条生效，322 条被保护性跳过，分布：
  - 248 条：目标父级已嵌套 / 是 entity / 已不存在（本设计禁止再挂深）；
  - 28 条：该标签自己已有子级（不许变三层）；
  - 46 条：子级在合并步骤已被并入其它标签。
- 结构完整性：三层链与 entity 作父级数量与基线一致（0 新增）。
- 示例（apply 后）：
  - `AI`：Hugging Face、Claude、Situational awareness…
  - `artificial intelligence`：OpenAI、ChatGPT…
  - `technology`：Anthropic、Meta、NVIDIA…
  - `AI模型`：DeepSeek…
  - `Middle East`：Iran、Israel、Gaza、Houthis、Hezbollah、Egypt、UAE…（23 个子级）

## 3. 需要人工复核的已知问题（都留在计划/报告里可改）

1. **个别 LLM 合并可疑**：如 `layoffs`←`rescue`、`nuclear weapons`←`nuclear`、`natural gas`←`LNG`。
   删掉对应 `groups` 条目再 apply 即可（apply 只执行计划里的条目）。
2. **跨批次同义碎档**：`洪水` 想挂到 `floods` 但 floods 已被并入其它标签等 46 条；下轮
   `tag tidy`（无 --auto-parents 小批量）可再收敛。
3. **基线遗留三层链**（economy→stock market→tariffs/earnings/IPO 等 5 条）与 entity 作父
   （China→Taiwan 等）本次有意未动，如需收敛另行处理。
4. 顶层还有大量 <10 篇的长尾叶子（一次性事件/人名），按设计不进层级。

## 4. 产物

- `backups/plan-auto-20260908.json` —— 完整可审/可编辑计划
- `backups/apply-report-20260908.txt` —— apply 明细（含全部 skipped 原因）
- `backups/papr-prod-snap-20260908.auto.db.gz` —— 已 apply 的库副本
- 本功能代码改动：`crates/papr-core/src/tag_taxonomy.rs`（`--auto-parents`：父级池、
  新父级规整 reconcile、apply 建父与两层约束）、`crates/papr-cli/src/main.rs`（CLI 参数），
  核心单测 10 个全绿。

## 5. 上线上一步（审阅后）

```bash
# 1) 先改计划（删除可疑合并、调 parent/name）
# 2) 对新快照副本 apply，复查 apply-report
# 3) 把 .auto.db 同步到线上（或对线上跑同一条 apply），重启 papr-server
```
