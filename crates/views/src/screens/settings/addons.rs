//! The addon sources page: the community addon servers the Addons provider searches, and the
//! dialog that checks a pasted address before it is stored.
//!
//! The list is the exact order a search walks, so the rows are labelled with their position.
//! The addresses these addons answer on are often temporary, which is the whole reason the
//! page exists: an expired link is deleted here and a fresh one pasted in its place.

use gpui::accesskit::Role;
use gpui::prelude::*;
use gpui::{AnyElement, Context, SharedString, div, px};
use i18n::t;
use router::{Destination, SettingsPage, navigate};
use state::Addons;
use ui::{ActiveTheme as _, Button, Modal, Switch, Text};

use super::rows::{Row, drill};
use super::{Group, SettingsView, Slot};

/// How wide the add dialog stands, matching the server sign-in dialog.
const PROMPT: f32 = 560.;

impl SettingsView {
    pub(super) fn addons_groups(&self) -> Vec<Group> {
        vec![Group::untitled().rows([Slot::AddonsList])]
    }

    /// The row on the Providers page that opens this page, saying how many addons there are
    /// and whether they are the source in use.
    pub(super) fn addon_sources_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let count = Addons::global(cx).read(cx).entries().len();
        let active = self.session.read(cx).provider_slug() == Some("addons");
        let value = match (count, active) {
            (0, _) => t!("settings-addons-none"),
            (count, true) => t!("settings-addons-count-active", count = count),
            (count, false) => t!("settings-addons-count", count = count),
        };

        drill("addon-sources", t!("settings-addons"), Some(value), cx)
            .on_click(|_, _, cx| navigate(Destination::Settings(SettingsPage::Addons), cx))
            .into_any_element()
    }

    /// The page: the lossless upgrade switch, then one row per addon with its own switch and
    /// remove button, then the button that opens the dialog for a new address.
    pub(super) fn addons_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let entries = Addons::global(cx).read(cx).entries().to_vec();
        let upgrade_on = self.playback.read(cx).lossless_upgrade();
        let upgrade = Row::new(
            t!("settings-addon-upgrade"),
            Switch::new("addon-upgrade", upgrade_on)
                .name(t!("settings-addon-upgrade"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.playback.update(cx, |playback, cx| {
                        playback.set_lossless_upgrade(!upgrade_on, cx)
                    });
                }))
                .into_any_element(),
        )
        .detail(t!("settings-addon-upgrade-detail"));

        let count = entries.len();
        let items = entries
            .into_iter()
            .enumerate()
            .map(|(index, entry)| self.addon_item((index, count), entry, cx))
            .collect::<Vec<_>>();

        let add = Button::new("addon-add")
            .label(t!("settings-addon-add"))
            .icon("icons/plus.svg")
            .small()
            .primary()
            .on_click(cx.listener(|this, _, _, cx| this.open_addon_prompt(cx)));

        let list = (!items.is_empty()).then(|| {
            div()
                .id("addon-list")
                .role(Role::List)
                .aria_label(t!("settings-addons"))
                .flex()
                .flex_col()
                .children(items)
        });
        let empty = list.is_none().then(|| {
            ui::spoken("addons-empty", t!("settings-addons-none"))
                .flex()
                .items_center()
                .h(theme.metrics.list_row)
                .text_color(theme.muted_foreground)
        });

        div()
            .flex()
            .flex_col()
            .child(upgrade)
            .children(list)
            .children(empty)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(theme.metrics.list_row)
                    .child(add),
            )
            .into_any_element()
    }

    /// One addon, placed by its index and the list's length: what it is called, the address it
    /// answers on, the switch that keeps it in or out of searches, and the button that removes
    /// it.
    fn addon_item(
        &self,
        (index, count): (usize, usize),
        entry: music::AddonEntry,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let name = entry.display_name();
        let url = SharedString::from(entry.url.clone());
        let enabled = entry.enabled;

        div()
            .id(SharedString::from(format!("addon-item-{index}")))
            .role(Role::ListItem)
            .aria_label(SharedString::from(name.clone()))
            .aria_position_in_set(index + 1)
            .aria_size_of_set(count)
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .h(theme.metrics.list_row)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .flex_1()
                    .child(
                        div()
                            .w(px(16.))
                            .flex_none()
                            .text_size(theme.text(Text::Tiny))
                            .text_color(theme.muted_foreground)
                            .child((index + 1).to_string()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .flex_1()
                            .child(div().truncate().child(SharedString::from(name.clone())))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(theme.text(Text::Small))
                                    .text_color(theme.muted_foreground)
                                    .child(url),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_1()
                    .child(
                        Button::new(SharedString::from(format!("addon-up-{index}")))
                            .ghost()
                            .icon("icons/chevron-up.svg")
                            .size_8()
                            .px_0()
                            .name(t!("settings-addon-move-up", name = &name))
                            .tooltip("settings-addon-move-up-short")
                            .tint(theme.muted_foreground)
                            .disabled(index == 0)
                            .on_click(cx.listener(move |_, _, _, cx| {
                                Addons::global(cx)
                                    .update(cx, |addons, cx| addons.move_up(index, cx));
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("addon-down-{index}")))
                            .ghost()
                            .icon("icons/chevron-down.svg")
                            .size_8()
                            .px_0()
                            .name(t!("settings-addon-move-down", name = &name))
                            .tooltip("settings-addon-move-down-short")
                            .tint(theme.muted_foreground)
                            .disabled(index + 1 == count)
                            .on_click(cx.listener(move |_, _, _, cx| {
                                Addons::global(cx)
                                    .update(cx, |addons, cx| addons.move_down(index, cx));
                            })),
                    ),
            )
            .child(
                Switch::new(
                    SharedString::from(format!("addon-enabled-{index}")),
                    enabled,
                )
                .name(t!("settings-addon-enabled", name = &name))
                .on_click(cx.listener(move |_, _, _, cx| {
                    Addons::global(cx)
                        .update(cx, |addons, cx| addons.set_enabled(index, !enabled, cx));
                })),
            )
            .child(
                Button::new(SharedString::from(format!("remove-addon-{index}")))
                    .ghost()
                    .icon("icons/trash-2.svg")
                    .size_8()
                    .px_0()
                    .name(t!("settings-addon-remove", name = &name))
                    .tooltip("settings-addon-remove-short")
                    .tint(theme.muted_foreground)
                    .on_click(cx.listener(move |_, _, _, cx| {
                        Addons::global(cx).update(cx, |addons, cx| addons.remove(index, cx));
                    })),
            )
            .into_any_element()
    }

    /// Opens the dialog for a fresh address, clearing whatever the last attempt left.
    pub(super) fn open_addon_prompt(&mut self, cx: &mut Context<Self>) {
        self.addon_prompt_open = true;
        self.addon_url
            .update(cx, |input, cx| input.set_text("", cx));
        Addons::global(cx).update(cx, |addons, cx| addons.clear_failure(cx));
        cx.notify();
    }

    /// Closes the dialog without adding anything.
    pub(super) fn close_addon_prompt(&mut self, cx: &mut Context<Self>) {
        self.addon_prompt_open = false;
        self.addon_url
            .update(cx, |input, cx| input.set_text("", cx));
        Addons::global(cx).update(cx, |addons, cx| addons.clear_failure(cx));
        cx.notify();
    }

    /// Hands the typed address to the provider, which checks it before storing it. The dialog
    /// stays up while the check runs, so a refusal can be answered on the spot.
    pub(super) fn submit_addon_prompt(&mut self, cx: &mut Context<Self>) {
        let url = self.addon_url.read(cx).text().trim().to_owned();
        if url.is_empty() {
            return;
        }
        let addons = Addons::global(cx);
        let pending = addons.read(cx).pending();
        if pending {
            return;
        }
        addons.update(cx, |addons, cx| addons.add(url, cx));
        // The dialog closes once the check has an answer; the entity holds the list, so
        // "added" needs nothing here. A failure keeps the dialog up through `failure()`.
        cx.notify();
    }

    /// The dialog that takes one address. Its confirm button follows the check: while one runs
    /// the button is held, and a refusal is shown under the field until another attempt or a
    /// dismissal.
    pub(super) fn addon_prompt(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let addons = Addons::global(cx);
        let pending = addons.read(cx).pending();
        let failure = addons.read(cx).failure().cloned();

        Modal::new("settings-addon-prompt", t!("settings-addon-add-title"))
            .w(px(PROMPT))
            .detail(t!("settings-addon-add-detail"))
            .child(self.addon_url.clone())
            .when_some(failure, |this, failure| {
                this.child(
                    div()
                        .mt_2()
                        .text_size(theme.text(Text::Small))
                        .text_color(theme.danger)
                        .child(failure),
                )
            })
            .action(
                Button::new("settings-addon-cancel")
                    .ghost()
                    .label(t!("common-cancel"))
                    .on_click(cx.listener(|this, _, _, cx| this.close_addon_prompt(cx))),
            )
            .action(
                Button::new("settings-addon-submit")
                    .label(match pending {
                        true => t!("settings-addon-checking"),
                        false => t!("settings-addon-submit"),
                    })
                    .primary()
                    .disabled(pending)
                    .on_click(cx.listener(|this, _, _, cx| this.submit_addon_prompt(cx))),
            )
            .on_dismiss(cx.listener(|this, _, _, cx| this.close_addon_prompt(cx)))
    }
}
