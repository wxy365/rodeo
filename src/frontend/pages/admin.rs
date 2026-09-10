use leptos::prelude::*;

use crate::frontend::icons::ic_add;

#[component]
pub fn Admin() -> impl IntoView {
    view! {
        <div class="page">
            <div class="crumb">"/admin · 仅系统管理员"</div>
            <div class="stats">
                <div class="panel stat"><div class="v">"—"</div><div class="l">"账号总数"</div></div>
                <div class="panel stat"><div class="v">"—"</div><div class="l">"工作空间"</div></div>
                <div class="panel stat"><div class="v">"—"</div><div class="l">"附件存储占用"</div></div>
            </div>
            <div class="panel set-body">
                <h2>"账号管理"</h2>
                <div class="invite">
                    <input class="inp" placeholder="搜索邮箱 / 姓名（即将上线）" disabled />
                    <button class="btn" disabled>"停用选中"</button>
                    <button class="btn pri" disabled>{ic_add()}"创建账号"</button>
                </div>
                <div class="empty">"系统管理功能即将上线，敬请期待"</div>

                <h2 style="margin-top:8px">"系统配置"</h2>
                <div class="cfg">
                    <div><b>"开放注册"</b><div class="mut">"关闭后仅系统管理员可创建账号"</div></div>
                    <button class="switch" disabled></button>
                </div>
                <div class="cfg">
                    <div><b>"附件大小限制"</b><div class="mut">"Entry 附件单文件上限，默认 50MB"</div></div>
                    <input class="inp" value="50 MB" disabled />
                </div>
                <div class="cfg">
                    <div><b>"会话过期时间"</b><div class="mut">"JWT 过期后需重新认证"</div></div>
                    <input class="inp" value="7 天" disabled />
                </div>
                <div class="cfg">
                    <div><b>"Workspace 回收站保留期"</b><div class="mut">"软删除后可恢复窗口"</div></div>
                    <input class="inp" value="30 天" disabled />
                </div>
                <div class="mut">"审计日志保留 180 天 · 全局操作流水见「审计日志」标签页"</div>
            </div>
        </div>
    }
}
