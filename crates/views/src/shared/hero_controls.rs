use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnimationExt as _, App, ClickEvent, SharedString, SpringAnimation, Window, div, px};
use input::Refresh;
use state::Sonora;
use ui::motion::Springs;
use ui::{ActiveTheme as _, Button, liquid_capsule};

/// The buttons a fullbleed page floats over its hero in place of the title bar: the sidebar
/// toggle, history, an optional refresh, and the right sidebar's toggle.
pub(crate) type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub(crate) struct HeroControls {
    id: &'static str,
    refresh: bool,
    on_sidebar: Option<ClickHandler>,
    on_queue: Option<ClickHandler>,
}

impl HeroControls {
    pub(crate) fn new(id: &'static str) -> Self {
        Self {
            id,
            refresh: false,
            on_sidebar: None,
            on_queue: None,
        }
    }

    /// Adds the refresh button to the history capsule.
    pub(crate) fn refresh(mut self) -> Self {
        self.refresh = true;
        self
    }

    pub(crate) fn on_sidebar(mut self, handler: ClickHandler) -> Self {
        self.on_sidebar = Some(handler);
        self
    }

    pub(crate) fn on_queue(mut self, handler: ClickHandler) -> Self {
        self.on_queue = Some(handler);
        self
    }
}

impl RenderOnce for HeroControls {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = *cx.theme();
        let navigation = router::trail(cx);
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
                Button::new(SharedString::from(format!("{}-back", self.id)))
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
                Button::new(SharedString::from(format!("{}-forward", self.id)))
                    .ghost()
                    .icon("icons/chevron-right.svg")
                    .tooltip("nav-forward")
                    .size_10()
                    .px_0()
                    .rounded_full()
                    .disabled(!can_forward)
                    .on_click(move |_, _, cx| forward.update(cx, |nav, cx| nav.forward(cx))),
            )
            .when(self.refresh, |this| {
                this.child(div().flex_none().w(px(1.)).h(px(14.)).bg(theme.border))
                    .child(
                        Button::new(SharedString::from(format!("{}-refresh", self.id)))
                            .ghost()
                            .icon("icons/refresh-cw.svg")
                            .tooltip("app-refresh")
                            .size_10()
                            .px_0()
                            .rounded_full()
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(Refresh), cx)
                            }),
                    )
            });

        // The sidebar toggle leads, hard against the sidebar it stands for. Its glyph swings
        // between the open and closed marks on the same spring the search field opens with,
        // so the two buttons in this row change the same way.
        let sidebar = self.on_sidebar.map(|handler| {
            liquid_capsule(cx).child(
                Button::new(SharedString::from(format!("{}-sidebar", self.id)))
                    .ghost()
                    .icon(match sidebar_open {
                        true => "icons/panel-left-close.svg",
                        false => "icons/panel-left-open.svg",
                    })
                    .tooltip("nav-sidebar")
                    .size_10()
                    .px_0()
                    .rounded_full()
                    .aria_expanded(sidebar_open)
                    .on_click(move |event, window, cx| handler(event, window, cx))
                    .with_spring(
                        SharedString::from(format!("{}-sidebar-swing", self.id)),
                        SpringAnimation::new(Springs::RESPONSIVE)
                            .to(match sidebar_open {
                                true => 1.,
                                false => 0.,
                            })
                            .with_epsilon(0.01),
                        |button, t| button.turned(t * (1. - t)),
                    ),
            )
        });
        let queue = self.on_queue.map(|handler| {
            liquid_capsule(cx).child(
                Button::new(SharedString::from(format!("{}-queue", self.id)))
                    .ghost()
                    .icon(match queue_open {
                        true => "icons/panel-right-close.svg",
                        false => "icons/panel-right-open.svg",
                    })
                    .tooltip("nav-sidebar-right")
                    .size_10()
                    .px_0()
                    .rounded_full()
                    .aria_expanded(queue_open)
                    .on_click(move |event, window, cx| handler(event, window, cx))
                    .with_spring(
                        SharedString::from(format!("{}-queue-swing", self.id)),
                        SpringAnimation::new(Springs::RESPONSIVE)
                            .to(match queue_open {
                                true => 1.,
                                false => 0.,
                            })
                            .with_epsilon(0.01),
                        |button, t| button.turned(t * (1. - t)),
                    ),
            )
        });

        div()
            .flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(sidebar)
                    .child(history),
            )
            .children(queue)
    }
}
