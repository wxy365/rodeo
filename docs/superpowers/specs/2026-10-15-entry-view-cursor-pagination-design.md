# 视图查询可扩展到 20M 条 — 设计（Phase 1：复合索引 + cursor 分页）

日期：2026-10-15

## 1. 范围

Phase 1 落地前一轮 spec 评估里点的 5 项改造中的 **#2（复合有序索引 + cursor 分页）** 与 **#3（count 快路径）**。这两项共同把视图查询从「不可用」拉到「可接受」——20M Entry 下首屏在秒级、翻页在几十毫秒级。

不在本轮：

- **#1 谓词下推到存储层**：依赖本轮的索引结构，但本身的改动是另一档量级（要动 `extract_index_hints` 之外的整条 evaluate 流水线，且要同时改 RocksDB 和 Postgres 后端的 `WHERE` 子句生成）。单独排期。
- **#4 审计 / 继承 / 账号懒加载**：本轮把 `query()` 换成 cursor 路径之后，**这部分工作量会变小**——因为 lazy 拉的对象范围从「全 workspace」缩到「本视图命中集」。等 #1 落地后再评估是否还有必要做。
- **Postgres 后端的本轮改造**：等 RocksDB 跑稳再跟进。CLAUDE.md 钉的约束——双后端接口——本轮仍保持，但只实现 `RocksDoc` 一侧；`PgDoc` 的 `scan_prefix` / `scan_range` 等新方法暂以「未实现 / 报错」占位，等跟进 PR 补。
- **Agent tool 路径**：`src/service/agent/tools/query_entries.rs` 当前直接调 `EntryService::query`，本轮在 `query()` 内部分流 cursor / page，对 Agent tool 是透明的；不动。

显式不做：

- 不做跨视图共享的排序预设（与 `2026-09-22-view-sort-and-ui-fixes-design.md` 同源决定）。
- 不为「任意标签可排序」开放全部能力：只有 schema 上标记 `sortable: true` 的标签才进有序索引。
- 不做 cursor 编码里的权限位（cursor 在 workspace 间不通用、不带租户上下文校验——后端在 resolver 层强校验 workspace 成员资格就够了）。

## 2. 设计决策（已确认）

| 决策 | 选择 | 理由 |
|------|------|------|
| 推进方式 | Phase 1 = #2 + #3 | #1/#4 都依赖本轮的索引与查询骨架 |
| Postgres | 先 RocksDB | CLAUDE.md 默认组合；PG 跟进排期 |
| API 兼容 | `page` 字段保留 + 新增 `cursor` / `limit` / `nextCursor` | 老客户端不破；新客户端走 cursor |
| 索引范围 | 固定一组：内建字段 + `sortable: true` 的标签 | 索引数量可控；产品侧「哪些标签可排序」本就该是 schema 决策 |
| Cursor 合法性 | 内嵌 `sort_spec_hash` + 服务端比对 | 排序变更 = 重置分页（Stripe / GitHub 同款） |

## 3. 数据结构改动

### 3.1 `LabelSchema` 新增 `sortable: bool`

`src/domain/label.rs:343` 的 `LabelSchema` 沿用追加模式（仓库里 `multi` / `format` / `default_value` / `links` 全是这种「末尾 + `#[serde(default)]`」的追加风格）：

```rust
// 追加在末尾
/// 本标签可作为视图排序键。`true` 时 `EntryService` 启动期会建立/校验
/// `(ws_id, encoded_value, code)` 的有序索引；视图排序命中该标签时
/// 走 cursor 分页路径。
#[serde(default)]
pub sortable: bool,
```

`serde(default)` 保证老记录（无此字段）反序列化时取 `false` —— bincode 的 `#[serde(default)]` 走的是 `Default::default()`，对 `bool` 即 `false`。这一点已在 `multi` / `format` / `links` 上验证过。

### 3.2 新列族（CF）

`src/storage/rocksdb.rs:9` 的 `ALL_CFS` 追加：

```rust
// 视图排序用的复合有序索引。每个 sortable 排序键一个 CF：
//   ENTRIES_IDX_UPDATED     —— (ws_id, ~updated_at_ms, code)        已建索引：默认排序
//   ENTRIES_IDX_CREATED     —— (ws_id, ~created_at_ms, code)        已建索引
//   ENTRIES_IDX_TITLE       —— (ws_id, title, code)                 已建索引
//   ENTRIES_IDX_LABEL_<n>   —— (ws_id, encoded_label_value, code)   按 sortable 标签动态建立
//
// 新加 sortable 标签时，CF 名字由 schema name 哈希到一个固定的桶号（见 §3.3）。
// RocksDB 列族必须在 open 期就声明，所以桶是「预分配的容量」而不是「按需新建」。
```

`cf` 常量在 `storage/doc.rs` 的 `pub mod cf` 里集中维护。**CF 数量上限的设计选择**：见 §3.3。

### 3.3 Sortable 标签 → CF 桶的分配

**问题**：CF 数量过多会让 RocksDB 启动变慢（每个 CF 一个文件句柄 + 一份元数据）。RocksDB 单库上千个 CF 实测仍可用，但 1 万+ 就明显劣化。

**方案**：预分配 N 个 `ENTRIES_IDX_LABEL_<i>` 桶（`i ∈ 0..N`），每个 sortable 标签通过 `hash(label_name) mod N` 选桶。同一桶里多个标签的索引按 `(ws_id, tag_name, encoded_value, code)` 复合键存；扫描时先按 `(ws_id, tag_name)` 前缀缩范围。

**N 的初值**：16。后续按实际 sortable 标签数量调：

- 8 个 sortable 标签 / 工作空间（典型上限） × N=16 → 实际占用 N 个 CF
- 启动开销：16 CF 的元数据读取，毫秒级
- 同一桶里多标签：扫描时多跳一层 tag_name 前缀，对单桶内标签 ≤ 几十的情形无感

CF 名规范：

```rust
pub const ENTRIES_IDX_LABEL_BUCKETS: [&str; 16] = [
    "ENTRIES_IDX_LABEL_00", "ENTRIES_IDX_LABEL_01", ... "ENTRIES_IDX_LABEL_15",
];
```

键编码：

```rust
// 同桶内多标签：(ws_id, tag_name, encoded_value, code)
// 单标签专用桶（默认排序三个内建字段）：(ws_id, encoded_value, code)
fn idx_label_key(ws: Ulid, tag: &str, encoded: &[u8], code: &str) -> Vec<u8>
fn idx_updated_key(ws: Ulid, ts_ms: i64, code: &str) -> Vec<u8>
fn idx_created_key(ws: Ulid, ts_ms: i64, code: &str) -> Vec<u8>
fn idx_title_key(ws: Ulid, title: &str, code: &str) -> Vec<u8>
```

降序字段（`updated_at` / `created_at`）用 `i64::MAX - ms` 编码成 8 字节大端，与 `storage/keys.rs:73` 的 `message_key` / `:92` 的 `audit_log_key` 一致——前缀扫描最新在前。

### 3.4 Cursor 结构

```rust
/// Opaque cursor。base64(JSON) 编码；客户端不可读、不应解析。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cursor {
    /// Cursor 所属 workspace。GraphQL resolver 收到 cursor 时校验「cursor.ws == 入参 ws」，
    /// 不一致直接拒绝 —— 防止跨 workspace 重放。
    pub ws: Ulid,
    /// SortSpec 的规范化哈希。请求里的 sort 必须哈希到同一值，否则 cursor 失效。
    pub sort_hash: u64,
    /// 各级排序键的最后看到值。多键排序时长度等于 SortSpec.keys.len()。
    /// 编码方式：内建字段用对应字节序、标签字段走 encode_label_value_for_index。
    pub last_values: Vec<CursorValue>,
    /// tiebreaker：在 (sort_values) 相同时，按 code 升序推进。
    pub last_code: String,
    /// 是否反向（用于 last/before 翻上一页）。本轮第一版只支持 forward（next 方向），
    /// 反向预留但不在 GraphQL 暴露。
    #[serde(default)]
    pub backward: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "k", content = "v")]
pub enum CursorValue {
    /// i64 毫秒（updated_at / created_at / 数值类标签）
    TsMs(i64),
    /// 字节串（标签值经 encode_label_value_for_index 编码后）
    Bytes(Vec<u8>),
    /// 字符串（title 等）
    Str(String),
}
```

`sort_hash` 的规范化：

```rust
fn sort_spec_hash(spec: &SortSpec) -> u64 {
    // 1. SortKey 按 field 名字典序排序 —— 同一语义的 sort 总是同序
    // 2. field 名字 + desc bool 拼成字符串，喂 fnv / xxhash
    // 字段顺序不规范化（多键排序的相对顺序影响结果），只对字段集合做集合哈希。
    // 实现：xxhash 整个 JSON(serde_json::to_value(spec))，不重新排序。
}
```

简化方案（第一版）：**直接 `xxhash3(canonical_json)`**，不重新排序键。理由：用户实际操作中很少会改字段顺序；改了就当 sort 变了，重置分页。多花一点客户端重置的成本，换实现简单。

### 3.5 `PageInput` → `QueryInput`

```rust
/// 老的 page/pageSize 字段保留向后兼容。
#[derive(Debug, Deserialize)]
pub struct QueryInput {
    pub page: Option<u32>,
    pub page_size: Option<u32>,
    /// 走 cursor 路径时传；与 page 二选一；同传时 cursor 优先。
    pub cursor: Option<String>,
    /// cursor 模式下的返回上限。默认 50，上限 200（与 pageSize 上限一致）。
    pub limit: Option<u32>,
}
```

`QueryResult`（或 `EntryPage` 的 GQL 投影）新增：

```rust
pub struct QueryResult {
    pub items: Vec<(Entry, Vec<Labeling>)>,
    pub total: usize,
    pub label_names: Vec<String>,
    /// 只要当前 sort 在索引内就非 None；page/cursor 模式下都返回。
    pub next_cursor: Option<String>,
}
```

GraphQL 层 `GqlEntryPage`（`src/api/graphql.rs`）相应新增 `nextCursor: String` 字段（可空，老客户端忽略）。

## 4. 存储层新增 API

`src/storage/doc.rs` 在 `DocStore` 上加：

```rust
/// 范围扫描：从 `start`（包含）起到 `end`（不包含）按 key 升序返回键值对。
/// 与 scan_prefix 的差别：scan_prefix 是「以 prefix 起、不写上界」，
/// scan_range 是「显式给上下界」，用于 cursor 续扫与 LIMIT 下推。
pub fn scan_range(
    &self,
    cf: &str,
    start: &[u8],
    end: &[u8],
    limit: usize,
) -> Result<Vec<(Vec<u8>, Vec<u8>)>, AppError>;

/// 范围扫描的反向版本（用于 desc 排序的 cursor 续扫、prev 翻页）。
pub fn scan_range_reverse(
    &self,
    cf: &str,
    start: &[u8],
    end: &[u8],
    limit: usize,
) -> Result<Vec<(Vec<u8>, Vec<u8>)>, AppError>;
```

实现：

- **`RocksDoc`**：用 `iterator_cf(IteratorMode::From(start, Forward))` + `take(limit)` + 不超 `end`。
  - 反向：`IteratorMode::From(end, Reverse)`，但 RocksDB 的 reverse iterator 在 LSM 上的语义略坑——必须先 seek 到 `end` 然后反向走。封装在 helper 里。
- **`PgDoc`**：本轮只补 `scan_range`（forward）的桩实现——`unimplemented!()` + 在启动期打 warn log。`PgDoc` 的同构 worker-thread 模式（见 `src/storage/pg.rs:1-19` 的注释）保证接口签名一致即可，下一轮 PR 跟进实现。

## 5. 服务层改动

### 5.1 `EntryService` 新方法：`query_indexed`

```rust
impl EntryService {
    /// 走新索引的 cursor 查询路径。当 sort 在索引内时优先调用。
    pub fn query_indexed(
        &self,
        ws: Ulid,
        query: &Query,
        sort: &SortSpec,
        input: &QueryInput,
    ) -> Result<QueryResult, AppError>;

    /// 老的 query() 路径保留：当 sort 不在索引内、或客户端传了 page 字段时走这里。
    /// 内部仍然全量加载 + 过滤；本轮不动它的算法，只在末尾加 cursor 计算（仅在 sort 在索引内时）。
    pub fn query(&self, ws: Ulid, query: &Query, sort: &SortSpec, input: QueryInput) -> Result<QueryResult, AppError>;
}
```

**路径选择规则**（在 GraphQL resolver 里）：

```rust
fn choose_path(sort: &SortSpec, input: &QueryInput, schemas: &[LabelSchema]) -> Path {
    if input.cursor.is_some() {
        return Path::Indexed;   // cursor 路径硬性要求索引
    }
    if sort_is_indexed(sort, schemas) {
        return Path::Indexed;   // 索引内排序走快路径（即使 client 传 page）
    }
    Path::Legacy                // 排序不在索引内，唯一的选项
}

fn sort_is_indexed(sort: &SortSpec, schemas: &[LabelSchema]) -> bool {
    sort.keys.iter().all(|k| match &k.field {
        SortField::UpdatedAt | SortField::CreatedAt | SortField::Title => true,
        SortField::CreatedBy | SortField::UpdatedBy => false,  // 暂不索引：依赖 account 解析
        SortField::Label(name) => schemas.iter()
            .any(|s| s.name == *name && s.sortable),
    })
}
```

### 5.2 `query_indexed` 算法

输入：`query`, `sort`, `input: QueryInput`（含 `cursor` 可选、`limit` 可选、`page` 可选）

```
sort_spec_hash = sort_spec_hash(sort)

# 1. 选主索引 CF
primary_cf = match sort.keys[0].field:
    UpdatedAt  -> cf::ENTRIES_IDX_UPDATED
    CreatedAt  -> cf::ENTRIES_IDX_CREATED
    Title      -> cf::ENTRIES_IDX_TITLE
    Label(name) -> label_bucket_cf(name)

direction = if sort.keys[0].desc { Desc } else { Asc }

# 2. 决定扫描起点
(start_key, is_cursor) = if let Some(c) = &input.cursor:
    decoded = decode_cursor(c)?
    if decoded.sort_hash != sort_spec_hash:
        return Err(InvalidQuery("排序已变更，请从头分页"))
    let tiebreak_value = sort_value_bytes(&sort.keys[0], &decoded.last_values[0])
    next_position(start_of_cf(ws, primary_cf), ws, tiebreak_value, decoded.last_code, direction)
else if input.page.unwrap_or(1) > 1:
    # page > 1：必须先扫到 (page-1)*limit 处拿到 cursor
    # 第一版实现：page > 1 时强行走 Legacy 路径并打 warn；
    # 服务端日志里提示客户端切到 cursor。Phase 2 再做 page→cursor 翻译。
    return legacy_path_with_warning(...)
else:
    (start_of_cf(ws, primary_cf), false)  # 从头开始

# 3. 流式扫描
limit = input.limit.unwrap_or(50).min(200)
mut matched = Vec::with_capacity(limit)
mut total = 0usize
loop {
    let batch = store.scan_range(primary_cf, &cursor, &end_of_cf(ws), BATCH_SIZE = 500)?
    for (k, _) in batch {
        let code = decode_code_from_idx_key(&k)?
        let entry = store.get(ENTRIES, &entry_by_workspace_key(ws, &code))?
        let labels = load_labels_for_entry(ws, &code)?  # 见 §5.4 懒加载
        if query.evaluate(&entry, &labels, &env) {
            total += 1
            if total > (input.page.unwrap_or(1) - 1) * limit && matched.len() < limit:
                matched.push((entry, labels))
            if matched.len() == limit:
                break
        }
    }
    if matched.len() == limit: break
    if batch.last().is_none(): break  # 扫到底了
}

# 4. 多键排序：内存里对 limit 条做 tiebreaker 排序
sort_within_page(&mut matched, &sort.keys[1..], ...)

# 5. 生成 next_cursor（如果 total > 当前页最后一个位置）
next_cursor = if has_more_in_index(primary_cf, ws, &matched.last()):
    Some(encode_cursor(...))
else: None

# 6. 同样的 label_names 汇总逻辑（沿用现有）
```

**关键点**：

- **流式扫描 + 提前终止**：不再 `Vec<Entry>` 全量入内存。20M 条下，扫到第 50 个匹配就停，磁盘 IO 与内存都按 page_size 计。
- **`total` 的代价**：要拿到精确 total 必须扫完整个索引（即所有匹配）。本轮的做法：扫到 `limit` 后**继续往后扫**，直到确认没有更多匹配或计数稳定在「没有新匹配」为止——这是 `total` 字段为侧栏计数准备的。
  - 优化：如果查询是「纯索引键过滤」（例如 `Status = Open` 走 status 索引），可以**先用索引计数，再分页**——但需要把 `query` 的过滤拆成「索引可覆盖的子集」与「需要在 evaluate 里验证的剩余子集」。第一版不做这个拆分；侧栏 count 仍是「扫到确认无新匹配」。Phase 2 再加。
- **`page > 1` 翻译**：第一版直接 fallback 到 Legacy 路径。Phase 2 改成「内部用 cursor 跳到 `(page-1)*limit`」。理由：第一版的 page 兼容性已经够老客户端用，cursor 是推荐路径。

### 5.3 `count()` 快路径

`src/service/entry.rs:726` 的 `count()` 当前直接复用 `query()` 拿 total。在新架构下：

```rust
pub fn count(&self, ws: Ulid, query: &Query) -> Result<usize, AppError> {
    // 空查询 / 仅 Archived=false 这种最常见情形：直接走 ENTRIES_BY_WORKSPACE 的计数。
    // 代价：scan_prefix 拿 workspace 下所有 key 数；纯 bincode key 字节，不反序列化。
    if query.is_simple_countable() {
        let count = self.store.scan_prefix(cf::ENTRIES_BY_WORKSPACE, &ws.to_bytes())?
            .len();
        return Ok(count);
    }
    // 复杂查询：复用 query_indexed 的 total —— 它本来就要扫完整索引。
    self.query_indexed(ws, query, &SortSpec::default(), &QueryInput {
        page: Some(1), page_size: Some(1), cursor: None, limit: Some(1),
    }).map(|r| r.total)
}
```

`is_simple_countable()` 的定义（保守）：查询为空（`{}` / `null`）、或仅含 `Archived = false` 这类「与存储层已索引状态一一对应」的过滤。

20M 下 ENTRIES_BY_WORKSPACE 前缀扫描的代价：仍是 O(N) 字节读取，~16 bytes/key × 20M = 320 MB 顺序读。**不算快**。Phase 2 可以加一个 `ENTRIES_COUNT_BY_WORKSPACE` CF 维护精确计数；本轮先这样。

### 5.4 懒加载 entry 主体的取舍

`query_indexed` 在索引扫描命中后需要反序列化 `Entry` 主键 + 该 entry 的所有 `Labeling`。这有两条路：

- **A：每个匹配反序列化一次**（代码 5.2 流程所示）—— N 次随机读，N = page_size
- **B：命中后批量拉 labels**——一次 scan `LABELINGS_BY_WORKSPACE` 把该 workspace 全量标签拉到 `HashMap<code, Vec<Labeling>>`，然后只取需要的 code

A 的代价：page_size = 50 时，50 次随机 IO 读 entry + 50 次随机 IO 读 labels（每 entry 平均 5-10 个 labeling）。在 RocksDB 上配合 block cache 通常 < 10ms；但冷启动 / 大 workspace 下会退化。

B 的代价：workspace 全量标签仍是 O(N) 反序列化。**绕回了本轮要解决的问题**。

**本轮选 A**。理由：

- page_size 固定 ≤ 200，随机 IO 有界
- 索引已经按 (sort_key, code) 顺序命中，page 内 locality 高
- 即使没有 block cache，200 次随机读也在百毫秒级（SSD）；HDD 下是问题，但 spec 没要求 HDD

### 5.5 Account 排序仍未索引

`SortField::CreatedBy` / `UpdatedBy` 的 cursor 化要建 `(ws_id, account_id, code)` 或 `(ws_id, account_name, code)` 的索引，且要处理 account id 与 name 的脱钩（用户改名字后索引要失效）。本轮**显式不做**——`sort_is_indexed` 对这两个字段返回 `false`，触发 Legacy 路径。

理由：

- 现有 sort UI 里 CreatedBy / UpdatedBy 已经较少使用
- 涉及 account 系统的连带改动（重命名监听、删除清理）超出本轮范围

## 6. 写路径改动（双写）

Entry 的写路径（`create` / `update` / `set_labels` / `archive` / `unarchive`）每次都要更新对应索引的条目。集中在 `EntryService` 里加一个内部方法：

```rust
fn write_index_keys(&self, ws: Ulid, entry: &Entry, labels: &[Labeling], schemas: &[LabelSchema], ops: &mut Vec<BatchOp>) {
    // updated_at / created_at / title 三个内建字段：总写
    ops.push(idx_updated_put(ws, entry.updated_at, &entry.code, &entry.archived_at));
    ops.push(idx_created_put(ws, entry.created_at, &entry.code, &entry.archived_at));
    ops.push(idx_title_put(ws, &entry.title, &entry.code, &entry.archived_at));
    // 每个 sortable 标签：如果该 entry 有该标签，写一条
    for schema in schemas.iter().filter(|s| s.sortable) {
        if let Some(l) = labels.iter().find(|l| l.label_name == schema.name) {
            if let Ok(encoded) = encode_label_value_for_index(&l.value, schema.value_type) {
                ops.push(idx_label_put(ws, &schema.name, &encoded, &entry.code, &entry.archived_at));
            }
        }
    }
}

fn delete_index_keys(&self, ws: Ulid, entry: &Entry, labels: &[Labeling], schemas: &[LabelSchema], ops: &mut Vec<BatchOp>) {
    // 对称：删除所有对应的 idx 条目
}
```

**archived_at 的处理**：当前索引按 `(ws_id, updated_at, code)` 排序，archived 条目仍占位置。视图默认看「未归档」——前端 UI 默认过滤；但本轮不在索引里区分归档与否，**在 evaluate 阶段过滤**。理由：

- 让 `archived_at = None` 这条过滤器跑 evaluate 是廉价的（每个 entry 一次字段比较）
- 不需要在索引里维护两套（archived / non-archived）——索引条目数翻倍、且要双写

Phase 2 评估：若 archived 占比 > 80%，把 archived 拆出独立索引。

**写路径原子性**：所有索引条目与原 `ENTRIES` / `LABELINGS` 写入走同一个 `BatchOp::write_batch`，沿用 `service/mod.rs` 现有批量事务。

### 6.1 Label schema 改动时的索引重建

新增 `sortable: true` 标签时（用户编辑 schema），要触发对应 workspace 的索引灌库：

```rust
impl LabelService {
    /// 当 schema.sortable 从 false 变 true 时调用。
    /// 全 workspace 灌库 (ws_id, label_name, encoded_value, code) 索引。
    pub async fn enable_sortable(&self, schema: &LabelSchema) -> Result<usize, AppError> {
        // 1. 拿到该 workspace 所有有该标签的 labeling
        // 2. 对每条 labeling 算 encoded_value
        // 3. 批量写 idx_label_<bucket> CF
        // 4. 返回写入条数
    }
}
```

`sortable` 从 `true` 变 `false`：**保留索引不删**（避免回切时重建成本）。索引条目不影响查询路径——`sort_is_indexed` 直接不返回 true，自然不走索引。下次开启时检查 CF 是否已有条目、有则跳过灌库。

## 7. 启动期灌库（idempotent backfill）

`src/service/mod.rs::Services::new` 末尾已有的 idempotent backfill 链追加一段：

```rust
// 新增：entry 排序索引灌库
repair_entry_sort_indexes(self.store.clone())?;
```

```rust
fn repair_entry_sort_indexes(store: Arc<DocStore>) -> Result<(), AppError> {
    // 对每个 workspace 的每个 sortable 标签 + 三个内建字段：
    //   IF 索引 CF 中 ws 前缀条目数 < ENTRIES_BY_WORKSPACE 中 ws 前缀条目数:
    //     灌库
    // ELSE 跳过
    
    // 实现：先列 workspaces（一次 scan_prefix WORKSPACES），
    //       对每个 ws_id 列 sortable 标签（label_schemas），
    //       对每个 (ws, sortable_field) 比对计数并按需灌库。
}
```

启动期灌库的代价（20M 条场景）：扫 3 个内建字段的索引 × N workspaces = 3 次顺序扫，**总 IO 量 ≈ 3 × ENTRIES 总 size**，SSD 下分钟级。sortable 标签索引按需灌。启动延迟从「秒级」变成「分钟级」——**需要文档化**（`docs/operations.md` 或 `config.example.toml` 注释）。

**回滚安全**：灌库期间服务正常处理请求。索引未灌完的 workspace 走 Legacy 路径（`sort_is_indexed` 返回 false）——但因为 sort 在索引内仍返回 true，会触发一次「索引不全 → 路径选错」？需要更细的状态：

```rust
fn sort_is_indexed(sort: &SortSpec, schemas: &[LabelSchema], store: &DocStore, ws: Ulid) -> bool {
    let primary = match &sort.keys.first().map(|k| &k.field) {
        Some(SortField::UpdatedAt) => cf::ENTRIES_IDX_UPDATED,
        Some(SortField::CreatedAt) => cf::ENTRIES_IDX_CREATED,
        Some(SortField::Title) => cf::ENTRIES_IDX_TITLE,
        Some(SortField::Label(name)) => match schemas.iter().find(|s| &s.name == name) {
            Some(s) if s.sortable => label_bucket_cf(&s.name),
            _ => return false,
        },
        _ => return false,
    };
    // 关键：检查该 ws 在该 CF 下的条目数 ≥ ENTRIES_BY_WORKSPACE 中 ws 的条目数
    // 不达标则返回 false，走 Legacy
    let idx_count = store.scan_prefix(primary, &ws.to_bytes()).map(|v| v.len()).unwrap_or(0);
    let ent_count = store.scan_prefix(cf::ENTRIES_BY_WORKSPACE, &ws.to_bytes()).map(|v| v.len()).unwrap_or(0);
    idx_count >= ent_count
}
```

`scan_prefix` 数条目仍是 O(N) 字节——每次 sort_is_indexed 判定都要扫两次！**缓存**：以 `(ws, primary_cf) -> bool` 为 key 在 `Services` 里维护一个 `Arc<RwLock<HashMap<...>>>`，命中即返回。写路径（灌库完成 / 新建 entry 触发 write_index_keys 后）更新缓存。

## 8. GraphQL 层改动

### 8.1 QueryInput 类型

`src/api/graphql.rs` 中 `entries(...)` resolver 入参改为：

```graphql
entries(
  workspaceId: ID!,
  viewId: ID,
  query: JSON,
  sorts: [SortInput!],
  page: Int,           # 老
  pageSize: Int,       # 老
  cursor: String,      # 新
  limit: Int           # 新
): EntryPage!
```

### 8.2 `EntryPage` 新字段

```graphql
type EntryPage {
  items: [Entry!]!
  total: Int!
  page: Int!
  pageSize: Int!
  labelNames: [String!]!
  nextCursor: String   # 新增；仅 sort 在索引内时有值
}
```

### 8.3 Resolver 分流

```rust
async fn entries(&self, ctx, workspace_id, view_id, query, sorts, page, page_size, cursor, limit) -> ... {
    // 1. 解析 Query / SortSpec（沿用现有逻辑）
    // 2. 加载 schemas（沿用现有逻辑）
    // 3. choose_path(sort, input, &schemas) 决定路径
    // 4. 分发到 query_indexed 或 query()
    // 5. 投影到 GqlEntryPage
}
```

### 8.4 Agent tool 透明

`src/service/agent/tools/query_entries.rs` 内部把 `page / page_size` 装成 `QueryInput`，直接复用 `EntryService::query` —— 不需要改 tool 自己的签名。

## 9. 前端改动

`src/frontend/graphql_client.rs` 的 `EntryPage` 结构与 `entries(...)` 函数：

- `EntryPage` 加 `next_cursor: Option<String>`
- `entries(...)` 接受 `cursor: Option<String>`、`limit: Option<u32>`
- `src/frontend/pages/workspace_main.rs` 列表翻页：检测 `next_cursor` —— 有就用 cursor 翻页；没有（如 Legacy 路径）退回 `page++`
- 视图侧栏计数 (`entryCount`) 由 `views(...)` resolver 返回，触发的是 `count()` —— count 已在 §5.3 处理

老浏览器 / 老 GraphQL cache：不传 cursor，行为完全保留。

## 10. 验证回路

按 CLAUDE.md 的「验证回路」：

```bash
make check      # native + wasm 两侧类型检查
make build      # 完整编译
```

**额外的人工验证步骤**（不入 make，因为涉及行为差异）：

1. **索引覆盖率 sanity check**：新启一个 workspace，写 10 条 entry，运行 `make dev`，手工开视图、点排序下拉 —— 验证 `nextCursor` 在默认排序下出现。
2. **Cursor 失效路径**：故意改视图排序，下一页 —— 验证返回 `InvalidQuery("排序已变更...")`，前端有提示。
3. **20M 模拟**（不强制跑）：用脚本灌 1M 条 entry 进 test workspace（受单机时间约束），验证：
   - 首屏 < 1s
   - 翻页 < 100ms
   - count() < 5s
4. **回滚**：在 `config.toml` 加 `[experimental] disable_cursor_path = true` —— 服务端强制走 Legacy 路径，验证不退化。

不回退到 §5.2 提到的 B 方案（workspace 全量标签拉一次）——理由在 §5.4。

## 11. 风险与缓解

| 风险 | 缓解 |
|------|------|
| CF 数量爆炸（用户加大量 sortable 标签） | N=16 桶上限；超 16 个 sortable 标签时 hash 撞桶；桶满时 RocksDB 单 CF 大小仍在可接受范围 |
| 老数据未灌库期间 sort_is_indexed 抖动 | 用 `Services` 内的 `RwLock<HashMap>` 缓存 (ws, cf) → is_ready；启动期灌完才解锁 |
| Cursor 被构造但对应 entry 已删除 | cursor 续扫时遇到「主表 get 返回 None」直接跳过；cursor 内的 last_code 不必精确指向存在的 entry |
| Cursor 跨 workspace 重放 | GraphQL resolver 层已强校验 workspace 成员；cursor 内嵌 workspace_id 字段冗余校验（加在 Cursor 结构里，§3.4 补） |
| 索引与主表不一致（写崩溃在 BatchOp 中间） | BatchOp 是 RocksDB WriteBatch 原子事务；要么全成要么全不成。不存在「主表写了一半、索引写了一半」 |
| Postgres 后端用户拿到 `unimplemented!()` panic | 启动期 `PgDoc::scan_range` 返回 `Err(AppError::Storage("cursor 路径暂不支持 Postgres"))`；`choose_path` 看到此错降级到 Legacy。**实现**：DocStore 的 `scan_range` 不 panic，而是返回 `Err`；resolver 收到 `Err` 时若 sort 不在索引内则走 Legacy，否则向上抛 `AppError`。 |

### 11.1 回滚路径

**回滚开关**（config）：`[query] force_legacy_path = true` —— `choose_path` 永远返回 Legacy。便于线上出问题时一键回退，不依赖代码回滚。

**索引数据**：回滚期间保留索引条目不回写也不删除；下次开启 cursor 路径时校验版本一致即可。

## 12. 排期（粗估）

| 步骤 | 内容 | 估时 |
|------|------|------|
| 1 | `cf` 常量 + `idx_*_key` 编码 + RocksDoc `scan_range` / `scan_range_reverse` | 0.5 天 |
| 2 | LabelSchema 加 `sortable` + `sort_spec_hash` + Cursor 编解码 | 1 天 |
| 3 | `EntryService::query_indexed` 实现 + 分流逻辑 | 2 天 |
| 4 | 写路径 `write_index_keys` / `delete_index_keys` 接入所有 BatchOp | 1 天 |
| 5 | `LabelService::enable_sortable` + 启动期灌库 | 1 天 |
| 6 | `count()` 快路径 + 索引覆盖率缓存 | 0.5 天 |
| 7 | GraphQL resolver 改 + 前端 cursor 翻页 | 1 天 |
| 8 | `make check` + `make build` + 手工验证 | 0.5 天 |
| 9 | 回滚开关 + 文档 | 0.5 天 |

合计 ~8 人天。**单 PR 推荐**：1-4 一个 PR，5-7 一个 PR，8-9 一个 PR。

## 13. 显式不做（重申）

- 不做 Postgres 后端的本轮实现（`scan_range` 在 `PgDoc` 返回 `Err`）。
- 不做 Account / User 类字段的索引（`SortField::CreatedBy` / `UpdatedBy`）。
- 不做 archived 与 non-archived 的索引分层（Phase 2 评估）。
- 不做 cursor 的 prev 翻页方向（结构预留，GraphQL 不暴露）。
- 不做 cursor 的加密签名（依赖 workspace 成员资格校验）。
- 不动 Agent tool 签名（透明）。
- 不重构 `query()` 的 evaluate 流水线（那是 #1 的范围）。
