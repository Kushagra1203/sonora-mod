//! The controls a page without a title bar draws in its place.
//!
//! The home page runs to the top of the window, so the title bar's buttons float over it
//! instead: the sidebar toggle and the history at the top left, the queue and lyrics toggle
//! at the top right, all on glass, the way an album or an artist page carries its own.

use gpui::prelude::*;
use gpui::{AnimationExt as _, AnyElement, App, Pixels, SpringAnimation, Window, div, px};
use router::trail;
use state::Sonora;
use ui::motion::Springs;
use ui::{ActiveTheme as _, Button, liquid_capsule};

/// The height of a row of floating controls, so a page can keep its own content clear of it.
pub(crate) const HEIGHT: Pixels = px(40.);

/// What the floating controls ask of the window: the two sidebars, which a page cannot reach
/// on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PageEvent {
    ToggleSidebar,
    ToggleSidebarRight,
}

/// The floating controls of a page that has no title bar. `prefix` keeps the ids apart
/// between pages, and `toggles` is told which of the two sidebars was asked for.
pub(crate) fn floating(
    prefix: &str,
    toggles: impl Fn(PageEvent, &mut Window, &mut App) + Clone + 'static,
    cx: &App,
) -> AnyElement {
    let theme = *cx.theme();
    let navigation = trail(cx);
    let (can_back, can_forward) = {
        let navigation = navigation.read(cx);
        (navigation.can_go_back(), navigation.can_go_forward())
    };
    let settings = Sonora::global(cx).settings.clone();
    let (sidebar_open, queue_open) = {
        let settings = settings.read(cx);
        (settings.sidebar_open(), settings.sidebar_right_open())
    };

    let back = navigation.clone();
    let forward = navigation.clone();
    let history = liquid_capsule(cx)
        .flex()
        .items_center()
        .child(
            Button::new(format!("{prefix}-back"))
                .ghost()
                .icon("icons/chevron-left.svg")
                .tooltip("nav-back")
                .size_10()
                .px_0()
                .rounded_full()
                .disabled(!can_back)
                .on_click(move |_, _, cx| back.update(cx, |nav, cx| nav.back(cx))),
        )
        .child(div().flex_none().w(px(1.)).h(px(14.)).bg(theme.border))
        .child(
            Button::new(format!("{prefix}-forward"))
                .ghost()
                .icon("icons/chevron-right.svg")
                .tooltip("nav-forward")
                .size_10()
                .px_0()
                .rounded_full()
                .disabled(!can_forward)
                .on_click(move |_, _, cx| forward.update(cx, |nav, cx| nav.forward(cx))),
        )
        .child(div().flex_none().w(px(1.)).h(px(14.)).bg(theme.border))
        .child(
            Button::new(format!("{prefix}-refresh"))
                .ghost()
                .icon("icons/refresh-cw.svg")
                .tooltip("app-refresh")
                .size_10()
                .px_0()
                .rounded_full()
                .on_click(|_, window, cx| window.dispatch_action(Box::new(input::Refresh), cx)),
        );

    // The sidebar toggle leads, hard against the sidebar it stands for. Its glyph swings
    // between the open and closed marks on the same spring the search field opens with, so
    // the two buttons in this row change the same way.
    let toggle = |open: bool, event: PageEvent| {
        let name = match event {
            PageEvent::ToggleSidebar => "sidebar",
            PageEvent::ToggleSidebarRight => "sidebar-right",
        };
        let icon = match (open, event) {
            (true, PageEvent::ToggleSidebar) => "icons/panel-left-close.svg",
            (false, PageEvent::ToggleSidebar) => "icons/panel-left-open.svg",
            (true, PageEvent::ToggleSidebarRight) => "icons/panel-right-close.svg",
            (false, PageEvent::ToggleSidebarRight) => "icons/panel-right-open.svg",
        };
        let tooltip = match event {
            PageEvent::ToggleSidebar => "nav-sidebar",
            PageEvent::ToggleSidebarRight => "nav-sidebar-right",
        };
        let toggles = toggles.clone();

        liquid_capsule(cx).child(
            Button::new(format!("{prefix}-{name}"))
                .ghost()
                .icon(icon)
                .tooltip(tooltip)
                .size_10()
                .px_0()
                .rounded_full()
                .aria_expanded(open)
                .on_click(move |_, window, cx| toggles(event, window, cx))
                .with_spring(
                    format!("{prefix}-{name}"),
                    SpringAnimation::new(Springs::RESPONSIVE)
                        .to(match open {
                            true => 1.,
                            false => 0.,
                        })
                        .with_epsilon(0.01),
                    |button, t| button.turned(t * (1. - t)),
                ),
        )
    };

    let sidebar = toggle(sidebar_open, PageEvent::ToggleSidebar);
    let queue = toggle(queue_open, PageEvent::ToggleSidebarRight);

    div()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(sidebar)
                .child(history),
        )
        .child(queue)
        .into_any_element()
}
