# Rodeo — 产品设计文档

> 版本: 1.0 | 日期: 2026-09-04 | 状态: Draft

## 1. 产品概述

Rodeo 是一个面向中型组织（50-500人）的任务/问题跟踪 Web 系统，支持多工作空间隔离、灵活的标签体系、实时协作和全文检索。系统以私有部署为主，零外部服务依赖。

### 1.1 核心价值

- **灵活建模**：通过标签系统而非硬编码字段来表达任务属性，用户可自定义标签定义
- **实时感知**：多人协作时实时看到数据变更和同事在线状态
- **快速检索**：全文检索覆盖标题、内容和标签，毫秒级响应
- **简单部署**：单二进制文件，嵌入式存储和搜索，无外部依赖

### 1.2 目标用户

- 中型组织的研发团队、项目管理团队
- 需要任务跟踪和缺陷管理的工程团队
- 需要私有部署、数据不出内网的组织

## 2. 用户角色

| 角色 | 范围 | 权限概要 |
|------|------|----------|
| 系统管理员 | 全局 | 管理账号、系统配置 |
| Owner | Workspace | 全部权限 + 删除 Workspace + 转让 Owner |
| Maintainer | Workspace | 邀请/移除成员 + 修改角色 + 所有 Worker 权限 |
| Worker | Workspace | 创建/修改 Entry + 打标签 + 上传附件 + 创建 View |
| Reader | Workspace | 只读浏览 Entry + 查看 View |

## 3. 功能规格

### 3.1 账号与认证

**内置账号登录**
- 邮箱 + 密码注册（系统管理员创建或开放注册，可配置）
- 密码强度校验：最少 8 位，含大小写和数字
- 密码以 Argon2 哈希存储

**OAuth2/OIDC 外部登录**
- 支持配置多个外部 Provider（如 GitHub、企业 OIDC）
- 标准 Authorization Code 流程
- 首次 OAuth 登录自动创建 Account，后续登录关联已有 Account
- 支持一个 Account 绑定多个 OAuth Provider

**会话管理**
- JWT 存储于 HttpOnly Secure SameSite cookie
- 支持主动登出和会话吊销
- Token 过期后需重新认证

### 3.2 工作空间（Workspace）

**创建与管理**
- 任何已登录用户可创建 Workspace，自动成为 Owner
- Workspace 属性：名称、URL slug（自动生成，可编辑）、描述
- Owner 可删除 Workspace（软删除，可恢复期可配置）

**成员管理**
- Owner/Maintainer 可通过邮箱邀请其他 Account
- 邀请时分配角色（Worker/Reader/Maintainer）
- 被邀请人接受后加入 Workspace
- Owner/Maintainer 可修改成员角色或移除成员
- Owner 可将 Owner 角色转给其他成员（自身降为 Maintainer）

### 3.3 内容（Entry）

**创建与编辑**
- Entry 核心字段：标题（必填）、详情（富文本，可选）
- 创建时自动生成 16 位全局唯一编码（大小写字母 + 数字）
- 支持修改标题和详情内容
- 编辑冲突检测：基于 `updated_at` 乐观并发，提示用户刷新

**生命周期**
- Entry 通过内置标签表达状态：
  - Task 标签：Open → InProgress → Done / Archived
  - Bug 标签：Open → Fixed / WontFix / Archived
- 状态变更走统一打标路径，审计日志自动覆盖
- 归档 Entry 不删除，从默认视图中过滤

**详情编辑器**
- 使用 @opentiny/tiny-editor 富文本编辑器
- 支持格式化文本、列表、代码块、图片插入、链接
- 编辑内容保存为 JSON 格式

### 3.4 标签系统

**标签定义（LabelSchema）**
- Workspace 级别，每个 Workspace 可定义多个标签
- 核心属性：
  - name：内部标识，创建后不可修改，Workspace 内唯一
  - title：显示名称，可修改
  - value_type：值类型 — Null / Boolean / Integer / Float / String / Enum
  - enum_values：Enum 类型的可选值列表
- 内置标签：创建 Workspace 时自动生成 Task（Enum）和 Bug（Enum）
- Maintainer+ 可创建自定义标签

**打标签（Labeling）**
- 一个 Entry 对同一 LabelSchema 最多一条 Labeling（当前值）
- 更新标签值为 upsert 操作
- 打标签时记录操作人和时间
- 多值场景（如指派多人）：value_type=String，存储 JSON 数组

### 3.5 视图（View）

**视图定义**
- 用户可创建和保存视图，包含过滤条件、排序和列配置
- 过滤条件支持：
  - 时间区间：创建时间、更新时间
  - 标签值匹配：等于、不等于、包含、区间
  - 指派人：等于指定 Account
- 排序：按任意字段或标签值升序/降序
- 列配置：选择在表格中展示哪些字段/标签列

**视图渲染**
- 前端表现为表格，每行一个 Entry，展示标题 + 选中标签列
- 点击行在右侧展开详情面板（半屏）
- 详情面板可全屏展开
- 视图支持共享给 Workspace 成员

### 3.6 附件（Attachment）

- Entry 可附加任意格式文件
- 文件大小限制：默认 50MB，可配置
- 支持上传、下载、删除
- 附件元数据（文件名、类型、大小、上传人、时间）存储在数据库
- 文件内容存储在本地文件系统，通过可扩展接口支持未来迁移到对象存储

### 3.7 实时协作

**在线状态**
- 进入 Workspace 后自动显示在线成员列表
- 显示头像、名称和状态标识
- 离开/断线后自动移除

**数据变更推送**
- Workspace 内任何 Entry 变更（创建/修改/标签变更）实时推送给所有在线成员
- 客户端收到推送后自动更新对应 Signal，UI 即时刷新
- 正在查看某 Entry 的用户收到"已被他人修改"提示

**编辑指示**
- 用户打开 Entry 编辑面板时，广播"正在编辑"状态
- 其他用户看到"XXX 正在编辑"指示
- 不阻止其他人编辑（乐观策略）

### 3.8 全文检索

- 搜索范围：Entry 标题 + 详情纯文本 + 标签值
- 支持 Workspace 内搜索和跨 Workspace 搜索
- 搜索结果按相关度排序
- 搜索与 View 过滤条件可组合使用

### 3.9 审计日志

- 所有数据变更操作自动记录审计日志
- 记录内容：操作类型、操作人、资源类型和 ID、变更前/后快照、时间
- Owner/Maintainer 可在 Workspace 设置中查看审计历史
- 系统管理员可查看全局审计日志

## 4. 页面与交互规格

### 4.1 页面列表

| 路径 | 页面 | 权限 |
|------|------|------|
| `/login` | 登录 | 未登录 |
| `/oauth/callback/:provider` | OAuth 回调 | 未登录 |
| `/workspaces` | Workspace 列表 | 已登录 |
| `/:workspace_slug` | Workspace 主页（默认 View） | 成员 |
| `/:workspace_slug/entry/:code` | Entry 详情全屏 | 成员 |
| `/:workspace_slug/settings` | Workspace 设置 | Maintainer+ |
| `/admin` | 系统管理 | 系统管理员 |

### 4.2 核心交互流程

**登录流程**
1. 用户选择内置登录或外部 OAuth Provider
2. 内置登录：输入邮箱 + 密码 → 验证 → 进入系统
3. OAuth：跳转授权 → 回调 → 自动创建/关联 Account → 进入系统

**Entry 创建流程**
1. 在 View 页面点击"新建"按钮
2. 填写标题（必填），选择初始标签值（如 Task=Open）
3. 保存后自动生成 code，Entry 出现在列表中
4. 可继续编辑详情、打标签、上传附件

**标签操作流程**
1. 在 Entry 详情面板的标签区域
2. 点击标签名称 → 弹出值选择/输入
3. 根据标签类型：Enum 选择下拉、Boolean 开关、String 输入框等
4. 保存后实时广播给其他在线成员

**View 使用流程**
1. 选择/创建 View → 配置过滤条件
2. 系统根据条件查询 Entry 列表，渲染表格
3. 点击行 → 右侧展示详情
4. 点击全屏按钮 → Entry 详情全屏展示
