use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use gpui::{Context, Entity, Task};
use music::{Genre, GenreDetail, GenreSection, MusicApi};
use tokio::task::AbortHandle;

use crate::{Io, Session, SessionEvent, join};

/// How many bare genres get a picture fetched for them, which covers the first screens of tiles.
const DRESSED: usize = 48;
/// How many of those pictures are asked for at once.
const AT_ONCE: usize = 4;
/// Where the pictures are remembered, under the provider's own slug.
const COVERS: &str = "genre-cover";
/// How long a remembered picture is taken for. A picture changes rarely, and fetching every
/// one of them again on every launch is what filled the heap.
const FRESH: Duration = Duration::from_secs(7 * 24 * 60 * 60);

pub struct Genres {
    genres: Rc<Vec<Genre>>,
    loading: bool,
    error: Option<String>,
    session: Entity<Session>,
    io: Io,
    /// The pictures that were fetched before, so they are not fetched again.
    cache: storage::Cache,
    task: Option<Task<()>>,
    /// Fetches the pictures of genres the provider listed without one.
    dressing: Option<Task<()>>,
}

impl Genres {
    pub fn new(session: Entity<Session>, io: Io, cx: &mut Context<Self>) -> Self {
        cx.subscribe(&session, |this, _, event, cx| match event {
            SessionEvent::SignedIn => this.load(cx),
            SessionEvent::SignedOut => {
                this.task = None;
                this.dressing = None;
                this.genres = Rc::new(Vec::new());
                this.loading = false;
                this.error = None;
                cx.notify();
            }
            SessionEvent::Reconnected | SessionEvent::WentOffline | SessionEvent::LocalChanged => {}
        })
        .detach();

        Self {
            genres: Rc::new(Vec::new()),
            loading: false,
            error: None,
            session,
            io,
            cache: storage::Cache::standard(),
            task: None,
            dressing: None,
        }
    }

    pub fn genres(&self) -> Rc<Vec<Genre>> {
        self.genres.clone()
    }

    pub fn adopt(&mut self, id: &str, cover: Option<String>, cx: &mut Context<Self>) {
        let Some(cover) = cover else {
            return;
        };
        let Some(genre) = Rc::make_mut(&mut self.genres)
            .iter_mut()
            .find(|genre| genre.id == id && genre.cover.is_none())
        else {
            return;
        };

        genre.cover = Some(cover);
        cx.notify();
    }

    pub fn forget(&mut self, id: &str, cx: &mut Context<Self>) {
        let kept = self.genres.len();
        Rc::make_mut(&mut self.genres).retain(|genre| genre.id != id);
        if self.genres.len() != kept {
            cx.notify();
        }
    }

    pub fn is_loading(&self) -> bool {
        self.loading
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn load(&mut self, cx: &mut Context<Self>) {
        if self.loading || !self.genres.is_empty() {
            return;
        }
        let Some(client) = self.session.read(cx).client() else {
            return;
        };

        self.loading = true;
        self.error = None;
        cx.notify();

        let io = self.io.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            let lister = client.clone();
            let loaded = join(io.spawn(async move { lister.genres().await })).await;

            this.update(cx, |this, cx| {
                this.loading = false;
                match crate::settled(loaded, cx) {
                    Ok(genres) => {
                        this.genres = Rc::new(genres);
                        this.dress(client, cx);
                    }
                    Err(reason) => this.error = Some(reason),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Fetches pictures for the first genres the provider listed without one, a few at a time,
    /// and puts each on its tile as it lands. A genre whose picture cannot be found keeps the
    /// placeholder.
    ///
    /// The pictures that were fetched before are put on their tiles first, so a launch that has
    /// seen them before does not ask for them again, and what is fetched now is remembered for
    /// the next one.
    fn dress(&mut self, client: Arc<dyn MusicApi>, cx: &mut Context<Self>) {
        let Some(provider) = self.session.read(cx).provider_slug() else {
            return;
        };
        let prefix = format!("{COVERS}/{provider}/");
        let cache = self.cache.clone();
        let io = self.io.clone();
        self.dressing = Some(cx.spawn(async move |this, cx| {
            let read = cache.clone();
            let read_prefix = prefix.clone();
            let remembered = io
                .spawn_blocking(move || read.read_prefix(&read_prefix))
                .await
                .ok()
                .and_then(|read| read.ok())
                .unwrap_or_default();
            let bare = this.update(cx, |this, cx| {
                for (key, cover, saved_at) in remembered {
                    if !fresh(saved_at) {
                        continue;
                    }
                    let Some(id) = key.strip_prefix(&prefix) else {
                        continue;
                    };
                    this.adopt(id, Some(cover), cx);
                }
                this.bare()
            });
            let Ok(bare) = bare else {
                return;
            };
            if bare.is_empty() {
                return;
            }

            for batch in bare.chunks(AT_ONCE) {
                let asked = batch.iter().map(|id| {
                    let client = client.clone();
                    let id = id.clone();
                    join(io.spawn(async move { client.genre_cover(&id).await }))
                });
                let found = futures::future::join_all(asked).await;
                let fetched: Vec<(String, String)> = batch
                    .iter()
                    .zip(found.iter())
                    .filter_map(|(id, cover)| match cover {
                        Ok(Some(cover)) => Some((id.clone(), cover.clone())),
                        _ => None,
                    })
                    .collect();
                let kept = this.update(cx, |this, cx| {
                    for (id, cover) in batch.iter().zip(found) {
                        match cover {
                            Ok(Some(cover)) => this.adopt(id, Some(cover), cx),
                            Ok(None) => {}
                            Err(error) => log::debug!("genres: no picture for {id}: {error:#}"),
                        }
                    }
                });
                if kept.is_err() {
                    return;
                }
                if fetched.is_empty() {
                    continue;
                }
                // What was fetched is remembered, so the next launch does not fetch it again.
                let cache = cache.clone();
                let prefix = prefix.clone();
                let now = now();
                drop(io.spawn_blocking(move || {
                    let entries = fetched
                        .into_iter()
                        .map(|(id, cover)| (format!("{prefix}{id}"), cover, now));
                    if let Err(error) = cache.write_many(entries) {
                        log::warn!("genres: cannot remember the pictures: {error:#}");
                    }
                }));
            }
        }));
    }

    /// The genres that have no picture yet, up to [`DRESSED`] of them.
    fn bare(&self) -> Vec<String> {
        self.genres
            .iter()
            .filter(|genre| genre.cover.is_none())
            .take(DRESSED)
            .map(|genre| genre.id.clone())
            .collect()
    }
}

/// Whether a remembered picture is still taken for.
fn fresh(saved_at: i64) -> bool {
    let now = now();
    now.saturating_sub(saved_at) < FRESH.as_secs() as i64
}

/// The time a picture was fetched at, in seconds since the epoch.
fn now() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or_default()
}

pub struct GenreDetails {
    id: Option<String>,
    detail: Option<Arc<GenreDetail>>,
    sections: Rc<Vec<GenreSection>>,
    loading: bool,
    error: Option<String>,
    session: Entity<Session>,
    genres: Entity<Genres>,
    io: Io,
    task: Option<Task<()>>,
    request: Option<AbortHandle>,
}

impl GenreDetails {
    pub fn new(
        session: Entity<Session>,
        genres: Entity<Genres>,
        io: Io,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.subscribe(&session, |this, _, event, cx| match event {
            SessionEvent::SignedIn => {
                if let Some(id) = this.id.clone() {
                    this.clear();
                    this.open(&id, cx);
                }
            }
            SessionEvent::SignedOut => {
                this.clear();
                cx.notify();
            }
            SessionEvent::Reconnected | SessionEvent::WentOffline | SessionEvent::LocalChanged => {}
        })
        .detach();

        Self {
            id: None,
            detail: None,
            sections: Rc::new(Vec::new()),
            loading: false,
            error: None,
            session,
            genres,
            io,
            task: None,
            request: None,
        }
    }

    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    pub fn name(&self) -> Option<&str> {
        self.detail.as_ref().map(|detail| detail.name.as_str())
    }

    pub fn sections(&self) -> Rc<Vec<GenreSection>> {
        self.sections.clone()
    }

    pub fn is_loading(&self) -> bool {
        self.loading
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn open(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.id.as_deref() == Some(id) && (self.loading || self.detail.is_some()) {
            return;
        }

        self.clear();
        self.id = Some(id.to_owned());
        let Some(catalog) = self.session.read(cx).catalog(id) else {
            cx.notify();
            return;
        };
        if let Some(detail) = catalog.peek_genre(id) {
            self.adopt(id, detail, cx);
            cx.notify();
            return;
        }
        self.load(id.to_owned(), false, cx);
    }

    /// Drops the page and fetches it again from the provider, past the catalog cache, so it
    /// shows its loading state until the new sections land.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.id.clone() else {
            return;
        };
        self.clear();
        self.id = Some(id.clone());
        self.load(id, true, cx);
    }

    /// Fetches the genre's sections. A `fresh` load drops the cached genre first, so the
    /// provider is asked again.
    fn load(&mut self, id: String, fresh: bool, cx: &mut Context<Self>) {
        let Some(catalog) = self.session.read(cx).catalog(&id) else {
            cx.notify();
            return;
        };

        self.loading = true;
        cx.notify();

        let request = self.io.spawn({
            let id = id.clone();
            async move {
                if fresh {
                    catalog.invalidate_genre(&id).await;
                }
                catalog.genre(&id).await
            }
        });
        self.request = Some(request.abort_handle());
        self.task = Some(cx.spawn(async move |this, cx| {
            let loaded = join(request).await;

            this.update(cx, |this, cx| {
                if this.id.as_deref() != Some(id.as_str()) {
                    return;
                }

                this.loading = false;
                this.request = None;
                match crate::settled(loaded, cx) {
                    Ok(detail) => this.adopt(&id, detail, cx),
                    Err(reason) => this.error = Some(reason),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn adopt(&mut self, id: &str, detail: Arc<GenreDetail>, cx: &mut Context<Self>) {
        let cover = detail.cover();
        self.genres
            .update(cx, |genres, cx| match detail.sections.is_empty() {
                true => genres.forget(id, cx),
                false => genres.adopt(id, cover, cx),
            });
        self.sections = Rc::new(detail.sections.clone());
        self.detail = Some(detail);
    }

    fn clear(&mut self) {
        self.task = None;
        if let Some(request) = self.request.take() {
            request.abort();
        }
        self.id = None;
        self.detail = None;
        self.sections = Rc::new(Vec::new());
        self.loading = false;
        self.error = None;
    }
}
