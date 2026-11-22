//! The addon sources the settings screen manages: the list, and the probe that checks a
//! pasted address before it is stored.
//!
//! The list itself lives in the addon provider's own accounts folder; this entity is the
//! screen's side of it, and a live session picks edits up on its next search. A probe is
//! network work, so it runs on the io pool and lands back here; the dialog above watches
//! [`Addons::pending`] and [`Addons::failure`] to know what to show.

use gpui::{App, Context, Entity, SharedString, Task};
use music::AddonEntry;

use crate::{Io, join};

/// Where the addon list is read, probed and written from the settings screen.
pub struct Addons {
    entries: Vec<AddonEntry>,
    /// The address a probe is running for, while the dialog waits on it.
    pending: Option<String>,
    /// Why the last probe failed, until it is dismissed or another attempt starts.
    failure: Option<SharedString>,
    task: Option<Task<()>>,
}

impl Default for Addons {
    fn default() -> Self {
        Self::new()
    }
}

impl Addons {
    pub fn new() -> Self {
        Self {
            entries: music::addon_list(),
            pending: None,
            failure: None,
            task: None,
        }
    }

    pub fn global(cx: &App) -> Entity<Self> {
        crate::Sonora::global(cx).addons.clone()
    }

    /// The configured addons, in the order they are tried.
    pub fn entries(&self) -> &[AddonEntry] {
        &self.entries
    }

    /// Whether a probe is running, which holds the dialog's confirm button.
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Why the last probe failed, for the dialog to show.
    pub fn failure(&self) -> Option<&SharedString> {
        self.failure.as_ref()
    }

    /// Forgets the last failure, so a reopened dialog starts clean.
    pub fn clear_failure(&mut self, cx: &mut Context<Self>) {
        if self.failure.take().is_some() {
            cx.notify();
        }
    }

    /// Probes `url` and, when an addon answers, stores it at the end of the list. The name
    /// the manifest gave is kept beside the address, so the row says something friendlier
    /// than the host.
    pub fn add(&mut self, url: String, cx: &mut Context<Self>) {
        let url = url.trim().to_owned();
        if url.is_empty() || self.pending.is_some() {
            return;
        }
        self.pending = Some(url.clone());
        self.failure = None;
        cx.notify();

        let io = Io::global(cx);
        self.task = Some(cx.spawn(async move |this, cx| {
            let probed = join(io.spawn({
                let url = url.clone();
                async move { music::addon_probe(&url).await }
            }))
            .await;
            this.update(cx, |this, cx| {
                this.pending = None;
                match probed {
                    Ok(probe) => {
                        let mut entry = AddonEntry::new(url);
                        entry.name = probe.name;
                        this.entries.push(entry);
                        this.store();
                    }
                    Err(error) => {
                        this.failure = Some(format!("{error:#}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Takes an addon out of the list. The indexes of the rest shift, which matches how the
    /// plan for a search built after this edit names them.
    pub fn remove(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.entries.len() {
            return;
        }
        self.entries.remove(index);
        self.store();
        cx.notify();
    }

    /// Moves an addon one place earlier in the order search walks.
    pub fn move_up(&mut self, index: usize, cx: &mut Context<Self>) {
        if index == 0 || index >= self.entries.len() {
            return;
        }
        self.entries.swap(index, index - 1);
        self.store();
        cx.notify();
    }

    /// Moves an addon one place later in the order search walks.
    pub fn move_down(&mut self, index: usize, cx: &mut Context<Self>) {
        if index + 1 >= self.entries.len() {
            return;
        }
        self.entries.swap(index, index + 1);
        self.store();
        cx.notify();
    }

    /// Switches an addon on or off. A disabled addon keeps its place and its address; search
    /// simply walks past it.
    pub fn set_enabled(&mut self, index: usize, enabled: bool, cx: &mut Context<Self>) {
        let Some(entry) = self.entries.get_mut(index) else {
            return;
        };
        if entry.enabled == enabled {
            return;
        }
        entry.enabled = enabled;
        self.store();
        cx.notify();
    }

    /// Writes the list back. A failure is logged rather than shown: the next edit tries
    /// again, and nothing the listener did is lost while the file cannot be written.
    fn store(&self) {
        if let Err(error) = music::addon_save(&self.entries) {
            log::warn!("addons: cannot save the list: {error:#}");
        }
    }
}
