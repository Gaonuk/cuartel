use crate::sidebar::Sidebar;
use crate::theme::Theme;
use crate::workspace::WorkspaceView;
use gpui::*;

pub struct CuartelApp {
    sidebar: Entity<Sidebar>,
    workspace: Entity<WorkspaceView>,
}

impl CuartelApp {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let sidebar = cx.new(|cx| {
            let mut sb = Sidebar::new(cx);
            sb.add_session(
                "demo-1".into(),
                "fix-auth-bug",
                "Pi",
                cx,
            );
            sb.add_session(
                "demo-2".into(),
                "add-dark-mode",
                "Claude Code",
                cx,
            );
            sb
        });

        let workspace = cx.new(|cx| WorkspaceView::new("fix-auth-bug", cx));

        Self {
            sidebar,
            workspace,
        }
    }
}

impl Render for CuartelApp {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();

        div()
            .id("cuartel-root")
            .flex()
            .size_full()
            .bg(rgb(theme.bg_primary))
            .font_family("IBM Plex Sans")
            .child(self.sidebar.clone())
            .child(self.workspace.clone())
    }
}
