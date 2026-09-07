# 标签层级设计说明与线上快照

> 快照日期：2026-09-02（线上 /product/papr/data/papr.db）。数据会随自动打标继续增长，
> 本文档给出的是**设计规则**（长期有效）和**当时各层的实际内容**（一次性快照）。

## 0. 设计总览

- **两套独立词表**（同一 `tags` 表，`kind` 区分）：
  - `interest`：用户/后台手工维护的**封闭主题列表**（14 个，见 §1）——平行于层级树，**不属于任何层级**；模型打标时只许从中“选”，不许造。
  - `ai`：模型自由生成的标签词表（线上 33,233 个）——**唯一有层级结构的词表**。
- **层级规则**（迁移 v32，`tags.parent_id` + `tags.tag_type`）：
  - 两层深：**第 0 层（顶层）= 无 `parent_id`**；**第 1 层（子级）= 有 `parent_id`**。
  - 单一父级：一个标签只能有一个父标签；**不允许孙级**（三层设计取舍：分类树不做深，避免失控；相关事件与实体分开挂）。
  - 类型 `tag_type ∈ {entity, topic}`：`entity` = 具体的人/地/机构（伊朗、肯尼亚、ChatGPT）；`topic` = 抽象主题/区域/类别（Big Tech、economy、能源）。
  - 语义约定：entity 尽量挂到区域/领域 topic 下；同级并列，不做多叉深层目录。
- **现状提醒**：层级只存在于数据层（API/UI 仍按扁平列表展示）；当前只有 99/33,233 个 AI 标签（0.3%）挂在父级下，且存在单子父级、中英父级重复等现象——属于“刚开始铺”的状态，不是最终结构。

## 1. interest（封闭主题列表，14 个）

| 主题词 | 关联文章数 |
|---|---:|
| AI、芯片及算力相关 | 3683 |
| 特朗普 | 3450 |
| 战争 | 2602 |
| 中美关系 | 2585 |
| 贸易战 | 2119 |
| 中东局势 | 2088 |
| 欧洲 | 1990 |
| 关键矿产 | 857 |
| 拉美 | 611 |
| 中日关系 | 574 |
| 非洲 | 485 |
| 中欧关系 | 444 |
| 中印关系 | 293 |
| 蒙古朝鲜中亚东南亚 | 244 |

## 2. AI 词表分层统计（线上快照）

| 指标 | 数量 |
|---|---:|
| AI 标签总数 | 33,233 |
| 第 0 层（顶层，无父级） | 33,134 |
| 第 1 层（有父级） | 99 |
| 有子级的父标签数 | 63 |
| 类型 entity | 135（其中顶层 82） |
| 类型 topic | 434（其中顶层 393） |
| 无类型（默认叶子，绝大多数为一次性长尾） | 32,664 |

## 3. 第 0 层（顶层）

### 3.1 充当“父级”的顶层标签（63 个，即层级骨架）

| 父标签 | 类型 | 子级数 | 自身关联文章 |
|---|---|---:|---:|
| Big Tech | topic | 6 | 45 |
| economy | topic | 6 | 966 |
| politics | topic | 5 | 840 |
| Middle East | topic | 4 | 606 |
| elections | topic | 4 | 270 |
| technology | topic | 4 | 797 |
| United States | - | 3 | 11 |
| international relations | topic | 3 | 47 |
| 美国 | entity | 3 | 297 |
| Europe | topic | 2 | 762 |
| global economy | topic | 2 | 33 |
| investors | topic | 2 | 48 |
| stock market | topic | 2 | 544 |
| trade policy | topic | 2 | 44 |
| 中东局势 | - | 2 | 44 |
| 美国政治 | topic | 2 | 182 |
| AI | - | 1 | 1599 |
| AI regulation | topic | 1 | 41 |
| Africa | - | 1 | 190 |
| Brazil | - | 1 | 192 |
| China | entity | 1 | 1284 |
| Eastern Europe | - | 1 | 3 |
| FIFA | entity | 1 | 72 |
| GDP | topic | 1 | 46 |
| Guggenheim | - | 1 | 6 |
| South America | - | 1 | 7 |
| US policy | topic | 1 | 80 |
| artificial intelligence | topic | 1 | 167 |
| capital markets | topic | 1 | 30 |
| central banks | topic | 1 | 38 |
| defense | topic | 1 | 218 |
| economics | topic | 1 | 36 |
| energy | topic | 1 | 425 |
| energy policy | topic | 1 | 43 |
| entertainment | topic | 1 | 61 |
| governance | topic | 1 | 45 |
| immigration policy | - | 1 | 9 |
| prediction market | topic | 1 | 40 |
| primary | topic | 1 | 43 |
| retail | topic | 1 | 231 |
| revenue | topic | 1 | 31 |
| robotics | topic | 1 | 38 |
| stocks | topic | 1 | 38 |
| trade | topic | 1 | 438 |
| weather | topic | 1 | 72 |
| 中国 | - | 1 | 96 |
| 半导体 | topic | 1 | 70 |
| 印度 | - | 1 | 76 |
| 央行 | - | 1 | 16 |
| 好莱坞 | - | 1 | 14 |
| 对冲基金 | - | 1 | 16 |
| 巴西 | entity | 1 | 40 |
| 日本 | entity | 1 | 219 |
| 日本政治 | topic | 1 | 72 |
| 欧洲 | - | 1 | 17 |
| 私募股权 | topic | 1 | 38 |
| 美国政府 | - | 1 | 3 |
| 英国王室 | - | 1 | 13 |
| 金融科技 | - | 1 | 15 |
| 银行 | - | 1 | 23 |
| 阿根廷 | - | 1 | 17 |
| 非洲 | - | 1 | 9 |
| 食品安全 | - | 1 | 10 |

### 3.2 顶层带类型的标签（475 个）——有语义标注但暂无子级

#### topic（393 个，按关联文章数降序，正文列出前 120，其余见附录文件）

| 标签 | 关联文章 |
|---|---:|
| economy | 966 |
| politics | 840 |
| technology | 797 |
| Europe | 762 |
| war | 667 |
| diplomacy | 620 |
| Middle East | 606 |
| UK | 528 |
| investment | 512 |
| regulation | 458 |
| energy | 425 |
| geopolitics | 394 |
| military | 359 |
| legal | 347 |
| health | 315 |
| climate change | 314 |
| elections | 270 |
| OpenAI | 265 |
| finance | 234 |
| retail | 231 |
| M&A | 220 |
| defense | 218 |
| immigration | 205 |
| labor market | 200 |
| security | 198 |
| obituary | 196 |
| shipping | 195 |
| data centre | 192 |
| sports | 191 |
| wildfire | 188 |
| startup | 187 |
| history | 185 |
| 美国政治 | 182 |
| US economy | 179 |
| corporate earnings | 179 |
| private equity | 177 |
| cybersecurity | 175 |
| social media | 175 |
| heatwave | 173 |
| supply chain | 168 |
| artificial intelligence | 167 |
| crypto | 166 |
| acquisition | 162 |
| fiscal policy | 157 |
| infrastructure | 156 |
| foreign policy | 146 |
| yen | 146 |
| telecom | 141 |
| investing | 140 |
| oil prices | 137 |
| national security | 134 |
| lawsuit | 129 |
| US-China relations | 124 |
| 关税 | 122 |
| healthcare | 109 |
| World Cup | 105 |
| Asia | 102 |
| crime | 99 |
| automotive | 97 |
| cyberattack | 97 |
| environment | 90 |
| gold | 90 |
| fashion | 83 |
| employment | 82 |
| US policy | 80 |
| privacy | 80 |
| policy | 78 |
| US-Iran | 76 |
| 洪水 | 74 |
| 网络安全 | 74 |
| 财报 | 74 |
| AI safety | 73 |
| budget | 72 |
| corporate governance | 72 |
| venture capital | 72 |
| weather | 72 |
| 日本政治 | 72 |
| CEO | 71 |
| ceasefire | 71 |
| streaming | 71 |
| corruption | 70 |
| drought | 70 |
| fintech | 70 |
| legislation | 70 |
| 债券市场 | 70 |
| 半导体 | 70 |
| 美国制裁 | 70 |
| children | 69 |
| 融资 | 69 |
| Ebola | 68 |
| hedge funds | 68 |
| 社交媒体 | 68 |
| 伊朗战争 | 67 |
| Southeast Asia | 66 |
| 投资 | 65 |
| memory chips | 64 |
| tech stocks | 64 |
| 无人机 | 64 |
| 贸易战 | 63 |
| AI infrastructure | 61 |
| Democratic primary | 61 |
| entertainment | 61 |
| 战争 | 61 |
| luxury | 56 |
| dollar | 52 |
| e-commerce | 52 |
| far-right | 51 |
| pharmaceuticals | 51 |
| US military | 50 |
| competition | 50 |
| water | 50 |
| construction | 49 |
| US-China | 48 |
| investors | 48 |
| international relations | 47 |
| 外交 | 47 |
| GDP | 46 |
| natural disaster | 46 |
| outbreak | 46 |
| trial | 46 |

### 3.3 顶层高频“无类型”标签（前 40，多为实际高频主题/人物，只是还没标类型、没挂层级）

| 标签 | 关联文章 |
|---|---:|
| AI | 1599 |
| US politics | 1245 |
| US | 994 |
| Iran | 891 |
| Russia | 707 |
| Japan | 694 |
| Trump | 579 |
| government | 285 |
| media | 252 |
| central bank | 208 |
| EU | 203 |
| Brazil | 192 |
| Africa | 190 |
| Indonesia | 169 |
| 人工智能 | 166 |
| US sanctions | 164 |
| Venezuela | 164 |
| education | 159 |
| Pakistan | 158 |
| France | 157 |
| bond market | 156 |
| Iran war | 156 |
| funding | 150 |
| currency | 149 |
| real estate | 148 |
| tax | 145 |
| Spain | 139 |
| agriculture | 132 |
| book review | 129 |
| travel | 126 |
| culture | 125 |
| 尼泊尔 | 124 |
| mining | 123 |
| migration | 119 |
| space | 119 |
| Supreme Court | 118 |
| manufacturing | 117 |
| 通胀 | 116 |
| 数据中心 | 114 |
| Italy | 113 |

> 顶层总数 33,134：除上述 63 骨架 + 475 带类型 + 40 高频外，其余约 3.2 万是**无类型、使用 0–1 次的叶子长尾**（一次性事件/人名拼写，正常存在，不进层级）。

## 4. 第 1 层（子级，99 个，按父级分组）

| 父级 | 子级 | 类型 | 子级关联文章 |
|---|---|---|---:|
| AI | Hugging Face | entity | 32 |
| AI regulation | AI investment | topic | 45 |
| Africa | Kenya | entity | 37 |
| Big Tech | Alphabet | entity | 35 |
| Big Tech | Netflix | entity | 43 |
| Big Tech | Samsung | entity | 43 |
| Big Tech | Shein | entity | 48 |
| Big Tech | TikTok | entity | 47 |
| Big Tech | innovation | topic | 45 |
| Brazil | Lula | entity | 34 |
| China | Taiwan | entity | 204 |
| Eastern Europe | Black Sea | - | 34 |
| Europe | Germany | entity | 280 |
| Europe | UK politics | topic | 275 |
| FIFA | Gianni Infantino | entity | 33 |
| GDP | jobs report | topic | 45 |
| Guggenheim | Mark Walter | entity | 33 |
| Middle East | Israel | entity | 282 |
| Middle East | Strait of Hormuz | topic | 308 |
| Middle East | West Bank | - | 34 |
| Middle East | 伊朗 | entity | 227 |
| South America | Chile | entity | 36 |
| US policy | US Treasury | entity | 75 |
| United States | New York City | entity | 36 |
| United States | Ohio | entity | 37 |
| United States | Washington | - | 36 |
| artificial intelligence | ChatGPT | entity | 30 |
| capital markets | bond markets | topic | 29 |
| central banks | Bank of Japan | entity | 39 |
| defense | drones | topic | 145 |
| economics | inequality | topic | 34 |
| economy | banking | topic | 296 |
| economy | inflation | topic | 533 |
| economy | interest rates | topic | 455 |
| economy | monetary policy | topic | 301 |
| economy | stock market | topic | 544 |
| economy | trade | topic | 438 |
| elections | Congress | entity | 179 |
| elections | Democratic Party | entity | 160 |
| elections | Senate | entity | 178 |
| elections | midterms | topic | 141 |
| energy | oil | topic | 532 |
| energy policy | energy transition | topic | 48 |
| entertainment | NFL | topic | 46 |
| global economy | China economy | topic | 31 |
| global economy | IMF | entity | 34 |
| governance | surveillance | topic | 48 |
| immigration policy | deportation | topic | 49 |
| international relations | Indo-Pacific | topic | 46 |
| international relations | allies | topic | 41 |
| international relations | conflict | topic | 44 |
| investors | market talk | topic | 48 |
| investors | valuation | topic | 49 |
| politics | Reform UK | entity | 56 |
| politics | election | topic | 558 |
| politics | fundraising | topic | 50 |
| politics | governance | topic | 45 |
| politics | primary | topic | 43 |
| prediction market | Kalshi | entity | 48 |
| primary | Senate race | topic | 45 |
| retail | Walmart | entity | 32 |
| revenue | profit | topic | 32 |
| robotics | humanoid robots | topic | 30 |
| stock market | IPO | topic | 384 |
| stock market | earnings | topic | 376 |
| stocks | S&P 500 | topic | 36 |
| technology | Anthropic | entity | 234 |
| technology | Meta | entity | 198 |
| technology | NVIDIA | entity | 238 |
| technology | semiconductors | topic | 293 |
| trade | tariffs | topic | 309 |
| trade policy | US tariffs | topic | 44 |
| trade policy | exports | topic | 50 |
| weather | evacuation | topic | 45 |
| 中东局势 | Gaza | entity | 130 |
| 中东局势 | 霍尔木兹海峡 | entity | 106 |
| 中国 | 香港 | - | 21 |
| 半导体 | Intel | entity | 21 |
| 印度 | RBI | entity | 22 |
| 央行 | 美联储 | entity | 164 |
| 好莱坞 | Hayden Panettiere | entity | 21 |
| 对冲基金 | Citadel | entity | 20 |
| 巴西 | Bolsonaro | entity | 22 |
| 日本 | LDP | entity | 22 |
| 日本政治 | 高市早苗 | entity | 69 |
| 欧洲 | Romania | entity | 22 |
| 私募股权 | Apollo | entity | 21 |
| 美国 | Federal Reserve | entity | 345 |
| 美国 | 南卡罗来纳州 | - | 21 |
| 美国 | 纽约 | entity | 22 |
| 美国政府 | FBI | entity | 21 |
| 美国政治 | Trump administration | entity | 383 |
| 美国政治 | 特朗普 | entity | 535 |
| 英国王室 | Prince Harry | entity | 21 |
| 金融科技 | Stripe | entity | 22 |
| 银行 | HSBC | entity | 21 |
| 阿根廷 | Milei | entity | 23 |
| 非洲 | Zimbabwe | entity | 22 |
| 食品安全 | Salmonella | entity | 20 |

## 5. 常见疑问

- **为什么没有第 2 层？** 设计上只做一层父子（两层的树）。想做“中东 → 伊朗 → 伊朗危机”需要三层，当前放不下；等价做法是 `中东(区域 topic)` 挂 `伊朗(entity)`、`中东局势(topic)` 等并列子级。
- **interest 里也有“中东局势”，AI 里也有“中东局势”，冲突吗？** 不冲突：两套词表同名不同 kind、互不干扰。interest 是订阅/兴趣主题，AI 词表里的是自动打标产物（且该名已被用作父级）。
- **子级都是对的吗？** 不全是。当前 63 个父级里有一批“单子父级”（GDP→jobs report 这类合理，纯单子收纳需再判断），也有中英重复父级（United States 与 美国、Europe 与 欧洲 并存）——属于 LLM 批量建层级的第一轮结果，建议后续按片区人工策展收敛。

## 附录 A：顶层 topic 全量（393 个，按关联文章数降序）

| 标签 | 关联文章 |
|---|---:|
| economy | 966 |
| politics | 840 |
| technology | 797 |
| Europe | 762 |
| war | 667 |
| diplomacy | 620 |
| Middle East | 606 |
| UK | 528 |
| investment | 512 |
| regulation | 458 |
| energy | 425 |
| geopolitics | 394 |
| military | 359 |
| legal | 347 |
| health | 315 |
| climate change | 314 |
| elections | 270 |
| OpenAI | 265 |
| finance | 234 |
| retail | 231 |
| M&A | 220 |
| defense | 218 |
| immigration | 205 |
| labor market | 200 |
| security | 198 |
| obituary | 196 |
| shipping | 195 |
| data centre | 192 |
| sports | 191 |
| wildfire | 188 |
| startup | 187 |
| history | 185 |
| 美国政治 | 182 |
| US economy | 179 |
| corporate earnings | 179 |
| private equity | 177 |
| cybersecurity | 175 |
| social media | 175 |
| heatwave | 173 |
| supply chain | 168 |
| artificial intelligence | 167 |
| crypto | 166 |
| acquisition | 162 |
| fiscal policy | 157 |
| infrastructure | 156 |
| foreign policy | 146 |
| yen | 146 |
| telecom | 141 |
| investing | 140 |
| oil prices | 137 |
| national security | 134 |
| lawsuit | 129 |
| US-China relations | 124 |
| 关税 | 122 |
| healthcare | 109 |
| World Cup | 105 |
| Asia | 102 |
| crime | 99 |
| automotive | 97 |
| cyberattack | 97 |
| environment | 90 |
| gold | 90 |
| fashion | 83 |
| employment | 82 |
| US policy | 80 |
| privacy | 80 |
| policy | 78 |
| US-Iran | 76 |
| 洪水 | 74 |
| 网络安全 | 74 |
| 财报 | 74 |
| AI safety | 73 |
| budget | 72 |
| corporate governance | 72 |
| venture capital | 72 |
| weather | 72 |
| 日本政治 | 72 |
| CEO | 71 |
| ceasefire | 71 |
| streaming | 71 |
| corruption | 70 |
| drought | 70 |
| fintech | 70 |
| legislation | 70 |
| 债券市场 | 70 |
| 半导体 | 70 |
| 美国制裁 | 70 |
| children | 69 |
| 融资 | 69 |
| Ebola | 68 |
| hedge funds | 68 |
| 社交媒体 | 68 |
| 伊朗战争 | 67 |
| Southeast Asia | 66 |
| 投资 | 65 |
| memory chips | 64 |
| tech stocks | 64 |
| 无人机 | 64 |
| 贸易战 | 63 |
| AI infrastructure | 61 |
| Democratic primary | 61 |
| entertainment | 61 |
| 战争 | 61 |
| luxury | 56 |
| dollar | 52 |
| e-commerce | 52 |
| far-right | 51 |
| pharmaceuticals | 51 |
| US military | 50 |
| competition | 50 |
| water | 50 |
| construction | 49 |
| US-China | 48 |
| investors | 48 |
| international relations | 47 |
| 外交 | 47 |
| GDP | 46 |
| natural disaster | 46 |
| outbreak | 46 |
| trial | 46 |
| Big Tech | 45 |
| strategy | 45 |
| G20 | 44 |
| JPMorgan | 44 |
| biotech | 44 |
| bond yields | 44 |
| gas prices | 44 |
| trade policy | 44 |
| Disney | 43 |
| energy policy | 43 |
| ethics | 43 |
| merger | 42 |
| productivity | 42 |
| AI regulation | 41 |
| Hormuz | 41 |
| cryptocurrency | 41 |
| football | 41 |
| intelligence | 41 |
| nuclear | 41 |
| settlement | 41 |
| summer | 41 |
| 经济 | 41 |
| AI芯片 | 40 |
| MAGA | 40 |
| higher education | 40 |
| market | 40 |
| prediction market | 40 |
| trade war | 40 |
| film review | 39 |
| peace talks | 39 |
| 供应链 | 39 |
| 气候变化 | 39 |
| 美国国债 | 39 |
| 英国政治 | 39 |
| ETF | 38 |
| central banks | 38 |
| drug trafficking | 38 |
| journalism | 38 |
| robotics | 38 |
| socialism | 38 |
| stocks | 38 |
| 日本经济 | 38 |
| 私募股权 | 38 |
| 美国经济 | 38 |
| art | 37 |
| civilian casualties | 37 |
| commodities | 37 |
| currency intervention | 37 |
| robotaxi | 37 |
| court ruling | 36 |
| economics | 36 |
| job market | 36 |
| opinion | 36 |
| software | 36 |
| tech industry | 36 |
| Kim Jong Un | 35 |
| US Navy | 35 |
| reform | 35 |
| LNG | 34 |
| wildlife | 34 |
| design | 33 |
| floods | 33 |
| global economy | 33 |
| advertising | 32 |
| baseball | 32 |
| consumer protection | 32 |
| partnership | 32 |
| payments | 32 |
| takeover | 32 |
| video games | 32 |
| war crimes | 32 |
| women | 32 |
| 救援 | 32 |
| South China Sea | 31 |
| confirmation | 31 |
| consumption tax | 31 |
| data centres | 31 |
| disarmament | 31 |
| disaster response | 31 |
| drone | 31 |
| escalation | 31 |
| financing | 31 |
| revenue | 31 |
| attorney general | 30 |
| capital markets | 30 |
| luxury goods | 30 |
| refugees | 30 |
| Jackson Hole | 29 |
| aging | 29 |
| autos | 29 |
| iron ore | 29 |
| pharma | 29 |
| renewable energy | 29 |
| COVID-19 | 28 |
| US stocks | 28 |
| airport | 28 |
| disinformation | 28 |
| military aid | 28 |
| poll | 28 |
| 人事变动 | 28 |
| 加密货币 | 28 |
| AI chips | 27 |
| AI stocks | 27 |
| AI、芯片及算力相关 | 27 |
| AI模型 | 27 |
| LGBTQ | 27 |
| Washington DC | 27 |
| arrest | 27 |
| ballistic missile | 27 |
| hacking | 27 |
| progressives | 27 |
| recall | 27 |
| rocket | 27 |
| vaccines | 27 |
| volatility | 27 |
| 旅游 | 27 |
| 经济政策 | 27 |
| 讣告 | 27 |
| 零售业 | 27 |
| AI startup | 26 |
| OPEC | 26 |
| US-Iran relations | 26 |
| alliance | 26 |
| billionaires | 26 |
| cost of living | 26 |
| economic data | 26 |
| RFK Jr. | 23 |
| 债券收益率 | 22 |
| Lee Jae-myung | 21 |
| US-Canada trade | 20 |
| US-Iran tensions | 20 |
| Warner Bros Discovery | 20 |
| fast food | 20 |
| ride-hailing | 20 |
| fast fashion | 19 |
| JD Vance | 17 |
| US-Canada relations | 17 |
| cybercrime | 17 |
| dealmaking | 17 |
| fixed income | 17 |
| open-source AI | 17 |
| U.S. Open | 16 |
| US-China competition | 16 |
| air conditioning | 16 |
| space exploration | 14 |
| Asia-Pacific | 13 |
| Warner Bros | 13 |
| spinoff | 13 |
| weight-loss drugs | 13 |
| US market | 12 |
| DSA | 11 |
| H-1B签证 | 11 |
| US-Japan relations | 11 |
| food delivery | 11 |
| open source | 11 |
| whistleblower | 11 |
| Bristol Myers Squibb | 10 |
| US-Iran war | 10 |
| airstrike | 10 |
| cost cutting | 10 |
| short selling | 10 |
| Jan. 6 | 9 |
| US-Iran conflict | 9 |
| D.C. | 8 |
| Eurozone | 8 |
| airstrikes | 8 |
| bond selloff | 8 |
| defense tech | 8 |
| healthcare policy | 8 |
| net zero | 8 |
| 9/11 | 7 |
| AI buildout | 7 |
| Exxon Mobil | 7 |
| India-China relations | 7 |
| US CPI | 7 |
| US soccer | 7 |
| USAID | 7 |
| birth rate | 7 |
| great power competition | 7 |
| skin care | 7 |
| weight loss | 7 |
| working class | 7 |
| US-Israel | 6 |
| anti-corruption | 6 |
| childcare | 6 |
| cyberwarfare | 6 |
| healthcare costs | 6 |
| AMC | 5 |
| HR | 5 |
| Kim Yo Jong | 5 |
| Livestream | 5 |
| Pony AI | 5 |
| US-China rivalry | 5 |
| US-Iran deal | 5 |
| anti-war | 5 |
| girls' education | 5 |
| late night TV | 5 |
| license plate readers | 5 |
| near miss | 5 |
| screentime | 5 |
| Medicare-for-All | 4 |
| Pepper…and Salt | 4 |
| US inflation data | 4 |
| electric shock gloves | 4 |
| foreign-exchange reserves | 4 |
| homebuilders | 4 |
| horseracing | 4 |
| lockup | 4 |
| re-election | 4 |
| sell-off | 4 |
| transatlantic alliance | 4 |
| working-class voters | 4 |
| Beijing Summit | 3 |
| Ben-Gvir | 3 |
| G2 | 3 |
| H3 rocket | 3 |
| Profit-taking | 3 |
| T. Rowe Price | 3 |
| US China tech rivalry | 3 |
| boy band | 3 |
| cyber attacks | 3 |
| deep sea mining | 3 |
| girls' trip | 3 |
| heat waves | 3 |
| home building | 3 |
| homebuilder | 3 |
| homebuilder sentiment | 3 |
| jury duty scam | 3 |
| lithium-metal battery | 3 |
| nation-building | 3 |
| nonprofit | 3 |
| open-source models | 3 |
| seatback screens | 3 |
| state-building | 3 |
| take-private | 3 |
| ultraprocessed food | 3 |
| Chanel No. 5 | 2 |
| Coyote v. Acme | 2 |
| Dunkin | 2 |
| Fed rate-hike | 2 |
| Haitian-American | 2 |
| MAGA Inc. | 2 |
| MGM | 2 |
| Reuters/Ipsos poll | 2 |
| U.S.-Iran peace deal | 2 |
| UK defense | 2 |
| bestseller | 2 |
| bird watching | 2 |
| cyberdefense | 2 |
| daycare | 2 |
| end of life | 2 |
| false flag | 2 |
| firestorms | 2 |
| fly-fishing | 2 |
| going-concern | 2 |
| high yield | 2 |
| license-plate cameras | 2 |
| mental-health startup | 2 |
| money market funds | 2 |
| pre-orders | 2 |
| price-fixing | 2 |
| red-light therapy | 2 |
| short-covering | 2 |
| standup comedy | 2 |
| stock sell-off | 2 |
| super-app | 2 |
| teachers' aides | 2 |
| ultra-rich | 2 |
| ultrawealthy | 2 |
| workers compensation | 2 |
| write-down | 2 |
| antiaging | 1 |
| profit-sharing | 1 |

## 附录 B：顶层 entity 全量（82 个，按关联文章数降序）

| 标签 | 关联文章 |
|---|---:|
| China | 1284 |
| India | 575 |
| Ukraine | 550 |
| 美国 | 297 |
| Australia | 275 |
| South Korea | 259 |
| Canada | 228 |
| 日本 | 219 |
| SpaceX | 218 |
| 俄罗斯 | 176 |
| North Korea | 166 |
| 乌克兰 | 153 |
| Andy Burnham | 141 |
| 加拿大 | 140 |
| Apple | 136 |
| Hong Kong | 125 |
| NATO | 120 |
| Thailand | 89 |
| Putin | 78 |
| Singapore | 78 |
| Bessent | 77 |
| Texas | 76 |
| Malaysia | 74 |
| Afghanistan | 73 |
| FIFA | 72 |
| 委内瑞拉 | 72 |
| Dolly Parton | 69 |
| 澳大利亚 | 69 |
| DOJ | 68 |
| Ceuta | 67 |
| UAE | 67 |
| 以色列 | 67 |
| 德国 | 62 |
| 欧盟 | 61 |
| Netanyahu | 49 |
| Todd Blanche | 48 |
| Hungary | 47 |
| Syria | 45 |
| Uber | 45 |
| Hamas | 44 |
| FDA | 42 |
| Donald Trump | 41 |
| Takaichi | 41 |
| Zambia | 41 |
| 巴西 | 40 |
| 法国 | 40 |
| 韩国 | 40 |
| 巴基斯坦 | 39 |
| Justice Department | 38 |
| Nigel Farage | 38 |
| Silicon Valley | 38 |
| Sweden | 38 |
| Paramount | 37 |
| Wisconsin | 37 |
| 最高法院 | 37 |
| Blackstone | 36 |
| Fauci | 36 |
| Microsoft | 36 |
| Morocco | 34 |
| Minnesota | 33 |
| Bank of England | 32 |
| Alibaba | 31 |
| Cambodia | 31 |
| Max Miller | 31 |
| Modi | 31 |
| Poland | 31 |
| Treasury | 31 |
| Moderna | 30 |
| Starlink | 30 |
| Labour | 29 |
| Peru | 29 |
| South Carolina | 29 |
| Hezbollah | 28 |
| 土耳其 | 28 |
| FTC | 27 |
| GOP | 27 |
| Goldman Sachs | 27 |
| 墨西哥 | 27 |
| CFTC | 26 |
| DeepSeek | 26 |
| Gulf states | 26 |
| Jason Arday | 26 |

## 附录 C：本地复现 SQL

```sql
-- 快照统计
SELECT 'level0', COUNT(*) FROM tags WHERE kind='ai' AND parent_id IS NULL;
SELECT 'level1', COUNT(*) FROM tags WHERE kind='ai' AND parent_id IS NOT NULL;
-- 全部子级按父级分组
SELECT p.name, c.name, c.tag_type FROM tags c JOIN tags p ON p.id=c.parent_id WHERE c.kind='ai' ORDER BY p.name, c.name;
-- 顶层带类型
SELECT name, tag_type FROM tags WHERE kind='ai' AND parent_id IS NULL AND tag_type IS NOT NULL ORDER BY name;
```
