1. 本项目是一个关于任务/问题跟踪的web项目，功能包括：
- 包含账号/登陆系统。
- 权限管控。
- 内容（任务/问题）管理。
- 多人实时在线协作。
- 支持全文检索

项目包含的实体有：
- Account，账号，用于登陆系统和追溯审计。
- Workspace，工作空间，隔离不同类的内容。一个Workspace可以加入多个Account。Workspace中有多种角色：Owner、Maintainer、Workder、Reader四种角色，Owner和Maintainer可以邀请其他Account加入Workspace，并为其分配角色。
- Entry，一条内容。包含三个核心字段：title - 内容的标题；code - 内容的全局唯一编码，自动生成，由大小写字母和阿拉伯数字构成，长度16；detail - 内容详情，富文本格式，通过opentiny/tiny-editor 浏览和编辑。
- Attachment，附件，附加在Entry上的任意格式文件。
- LabelSchema，标签字段定义，包含核心字段：name - 标签名称，创建后不可编辑，在Workspace内具有唯一性；title - 显示名称；value_type - 标签值的类型，Null、Boolean、Integer、Float、String、Enum。每个Workspace内置name为Task和Bug的两个标签定义，在创建workspace时自动生成。
- Labeling，打标签，即维护Entry和LabelSchema的关联关系，包含核心字段：entry_code, label_name, label_value。
- View，视图，由用户定义内容过滤条件，包括：时间区间、标签匹配、Account。在前端表现为一个表格，表格罗列Entry实例，点击条目时可在右侧展示详情，也可进一步全屏展示详情。

技术栈：
- rust
- websocket
- ts
- tiny-editor
- 单体应用，前后端不分离
- 使用高性能rust web框架
- 全文检索（优先使用高性能轻量化的开源方案）
- 文档数据库