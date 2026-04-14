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

        let workspace = cx.new(|cx| {
            let ws = WorkspaceView::new("fix-auth-bug", cx);
            ws.terminal().update(cx, |term, cx| {
                term.push_output("cuartel v0.1.0 - agent orchestration platform\n", cx);
                term.push_output("connecting to rivet sidecar at localhost:6420...\n", cx);
                term.push_output("\n", cx);
                term.push_output(
                    "  sessions: 2 active | server: this mac (local)\n",
                    cx,
                );
                term.push_output(
                    "  agents: Pi, Claude Code | status: ready\n",
                    cx,
                );
                term.push_output("\n", cx);
                term.push_output("ready. press Cmd+T to open a new agent tab.\n", cx);
            });
            ws
        });

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
