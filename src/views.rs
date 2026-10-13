use super::*;
use gpui::{
    AnyElement, Div, ElementId, MouseButton, MouseDownEvent, SharedString, Stateful, StyledImage,
    anchored, deferred, div, img, prelude::*, rgb, uniform_list,
};
use settings::{Layout, size_text};
use std::ops::Range;

const SIDEBAR: f32 = 208.;
const LIST_ROW: f32 = 96.;
const CARD_WIDTH: f32 = 168.;
const GRID_GAP: f32 = 12.;
const GRID_ROW: f32 = 310.;
const WHITE: u32 = 0xffffff;

pub enum Primary {
    Fetch,
    Reveal(PathBuf),
    Retry(usize),
    None,
}

struct RowInfo {
    chip: Option<(&'static str, u32)>,
    label: &'static str,
    primary: Primary,
    enabled: bool,
    status: Option<String>,
    error: bool,
    progress: Option<f32>,
}

pub fn columns(window: &Window, grid: bool) -> usize {
    if !grid {
        return 1;
    }
    let width = f32::from(window.viewport_size().width) - SIDEBAR - 48.;
    ((width + GRID_GAP) / (CARD_WIDTH + GRID_GAP))
        .floor()
        .max(1.) as usize
}

fn styled_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    enabled: bool,
    background: u32,
    hover: u32,
    foreground: u32,
) -> Stateful<Div> {
    div()
        .id(id)
        .px_4()
        .py_2()
        .rounded_lg()
        .flex_shrink_0()
        .bg(rgb(background))
        .text_color(rgb(foreground))
        .text_sm()
        .when(enabled, |element| {
            element.cursor_pointer().hover(|style| style.bg(rgb(hover)))
        })
        .when(!enabled, |element| {
            element.opacity(0.45).cursor(gpui::CursorStyle::Arrow)
        })
        .child(label.into())
}

fn button(
    t: &Theme,
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    enabled: bool,
) -> Stateful<Div> {
    styled_button(id, label, enabled, t.button, t.button_hover, t.text)
}

fn primary(
    t: &Theme,
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    enabled: bool,
) -> Stateful<Div> {
    styled_button(id, label, enabled, t.accent, t.accent, WHITE)
}

fn cover(path: Option<PathBuf>, side: f32, t: &Theme) -> AnyElement {
    let placeholder = rgb(t.button);
    let size = px(side);
    match path {
        Some(path) => img(path)
            .size(size)
            .flex_shrink_0()
            .rounded_md()
            .object_fit(gpui::ObjectFit::Cover)
            .with_fallback(move || {
                div()
                    .size(size)
                    .rounded_md()
                    .bg(placeholder)
                    .into_any_element()
            })
            .into_any_element(),
        None => div()
            .size(size)
            .flex_shrink_0()
            .rounded_md()
            .bg(placeholder)
            .into_any_element(),
    }
}

fn logo(side: f32) -> AnyElement {
    let bundled = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join("../Resources/logo.png")))
        .filter(|path| path.is_file());
    let path = bundled
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/AppIcon.png"));
    img(path).size(px(side)).flex_shrink_0().into_any_element()
}

fn checkbox(t: &Theme, on: bool) -> Div {
    div()
        .size(px(18.))
        .flex_shrink_0()
        .rounded(px(4.))
        .border_1()
        .border_color(rgb(if on { t.accent } else { t.muted }))
        .flex()
        .items_center()
        .justify_center()
        .when(on, |element| {
            element
                .bg(rgb(t.accent))
                .text_color(rgb(WHITE))
                .text_xs()
                .child("✓")
        })
}

fn progress_bar(t: &Theme, fraction: f32) -> Div {
    div()
        .h(px(4.))
        .w_full()
        .flex_shrink_0()
        .rounded_full()
        .bg(rgb(t.button))
        .child(
            div()
                .h_full()
                .w(gpui::relative(fraction.clamp(0., 1.)))
                .rounded_full()
                .bg(rgb(t.ok)),
        )
}

fn chip(t: &Theme, text: &'static str, color: u32) -> Div {
    div()
        .flex_shrink_0()
        .px_2()
        .py_1()
        .rounded_md()
        .bg(rgb(t.bg))
        .text_xs()
        .text_color(rgb(color))
        .child(text)
}

fn meta_line(book: &LibraryBook) -> String {
    let mut parts = vec![];
    if !book.series.is_empty() {
        parts.push(if book.sequence.is_empty() {
            book.series.clone()
        } else {
            format!("{} #{}", book.series, book.sequence)
        });
    }
    parts.push(book.length());
    if let Some(bytes) = book.bytes {
        parts.push(format!("≈ {}", size_text(bytes)));
    }
    parts.push(match book.kbps {
        Some(kbps) => format!("AAX {kbps} kbps"),
        None => "AAXC".into(),
    });
    if !book.purchased.is_empty() {
        parts.push(format!("Bought {}", book.purchased));
    }
    parts.join(" · ")
}

fn byline(book: &LibraryBook) -> String {
    match (book.authors.is_empty(), book.narrators.is_empty()) {
        (_, true) => book.authors.clone(),
        (true, false) => format!("Narrated by {}", book.narrators),
        (false, false) => format!("{} · Narrated by {}", book.authors, book.narrators),
    }
}

fn segmented<T: Copy + PartialEq + 'static>(
    t: &Theme,
    id: &'static str,
    options: &[(T, &'static str)],
    current: T,
    cx: &mut Context<Earshelf>,
    apply: fn(&mut Earshelf, T),
) -> Div {
    let (selected, text) = (t.selected, t.text);
    div()
        .flex()
        .flex_shrink_0()
        .rounded_lg()
        .bg(rgb(t.button))
        .p_1()
        .gap_1()
        .children(options.iter().enumerate().map(|(index, &(value, label))| {
            div()
                .id((id, index))
                .px_3()
                .py_1()
                .rounded_md()
                .text_sm()
                .text_color(rgb(text))
                .cursor_pointer()
                .when(value == current, |element| element.bg(rgb(selected)))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    apply(this, value);
                    this.save_settings();
                    this.cursor = None;
                    cx.notify();
                }))
        }))
}

fn toggle(
    t: &Theme,
    id: &'static str,
    on: bool,
    cx: &mut Context<Earshelf>,
    apply: fn(&mut Earshelf),
) -> Stateful<Div> {
    div()
        .id(id)
        .w(px(40.))
        .h(px(22.))
        .flex_shrink_0()
        .p(px(2.))
        .rounded_full()
        .cursor_pointer()
        .flex()
        .bg(rgb(if on { t.accent } else { t.button_hover }))
        .when(on, |element| element.justify_end())
        .child(div().size(px(18.)).rounded_full().bg(rgb(WHITE)))
        .on_click(cx.listener(move |this, _, _, cx| {
            apply(this);
            this.save_settings();
            cx.notify();
        }))
}

fn setting(t: &Theme, title: &'static str, detail: String, control: impl IntoElement) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .py_3()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().child(title))
                .child(div().text_sm().text_color(rgb(t.muted)).child(detail)),
        )
        .child(control)
}

impl Earshelf {
    pub fn row_primary(&self, asin: &str) -> Primary {
        self.library
            .iter()
            .flatten()
            .find(|book| book.asin == asin)
            .map_or(Primary::None, |book| {
                self.row_info(&Theme::DARK, book).primary
            })
    }

    fn row_info(&self, t: &Theme, book: &LibraryBook) -> RowInfo {
        let idle = self.operation != Some(Operation::LoggingOut);
        let info = |chip, label, primary, enabled| RowInfo {
            chip,
            label,
            primary,
            enabled: enabled && idle,
            status: None,
            error: false,
            progress: None,
        };
        let reveal = |path: &Option<PathBuf>| path.clone().map_or(Primary::None, Primary::Reveal);
        match self.queue_index(&book.asin).map(|i| (i, &self.books[i])) {
            Some((_, queued)) if queued.state == State::Running => RowInfo {
                status: Some(queued.detail.clone()),
                progress: Some(queued.progress),
                ..info(Some(("Working", t.warn)), "Working…", Primary::None, false)
            },
            Some((_, queued)) if queued.state == State::Queued => info(
                Some(("In queue", t.muted)),
                "In queue",
                Primary::None,
                false,
            ),
            Some((_, queued)) if queued.state == State::Done => info(
                Some(("✓ Converted", t.ok)),
                "Show in Finder",
                reveal(&queued.output),
                true,
            ),
            Some((index, queued)) => RowInfo {
                status: Some(queued.detail.clone()),
                error: queued.state == State::Failed,
                ..info(
                    Some((
                        if queued.state == State::Failed {
                            "Failed"
                        } else {
                            "Cancelled"
                        },
                        t.error,
                    )),
                    "Retry",
                    Primary::Retry(index),
                    true,
                )
            },
            None if book.output.is_some() => info(
                Some(("✓ Converted", t.ok)),
                "Show in Finder",
                reveal(&book.output),
                true,
            ),
            None if book.source.is_some() => info(
                Some(("Downloaded", t.muted)),
                "Convert",
                Primary::Fetch,
                self.dependencies,
            ),
            None => info(
                None,
                "Download & convert",
                Primary::Fetch,
                self.dependencies,
            ),
        }
    }

    fn library_menu(&self, asin: &str) -> Vec<(&'static str, MenuAction)> {
        let Some(book) = self.library.iter().flatten().find(|b| b.asin == asin) else {
            return vec![];
        };
        let mut items = vec![];
        match self.row_info(&Theme::DARK, book).primary {
            Primary::Reveal(path) => items.push(("Show in Finder", MenuAction::Reveal(path))),
            Primary::Retry(index) => items.push(("Retry", MenuAction::Retry(index))),
            Primary::Fetch => items.push((
                if book.source.is_some() {
                    "Convert"
                } else {
                    "Download & convert"
                },
                MenuAction::Download(asin.to_owned()),
            )),
            Primary::None => {}
        }
        items.push(("Copy title", MenuAction::CopyTitle(book.title.clone())));
        items
    }

    fn queue_menu(&self, index: usize) -> Vec<(&'static str, MenuAction)> {
        let Some(book) = self.books.get(index) else {
            return vec![];
        };
        let mut items = vec![];
        if let Some(path) = &book.output {
            items.push(("Show in Finder", MenuAction::Reveal(path.clone())));
        }
        if matches!(book.state, State::Failed | State::Cancelled) {
            items.push(("Queue again", MenuAction::Retry(index)));
        }
        if book.state != State::Running {
            items.push(("Remove from queue", MenuAction::Remove(index)));
        }
        items.push(("Copy title", MenuAction::CopyTitle(book.title.clone())));
        items
    }

    fn open_menu(
        &mut self,
        event: &MouseDownEvent,
        items: Vec<(&'static str, MenuAction)>,
        cx: &mut Context<Self>,
    ) {
        self.menu = (!items.is_empty()).then_some(ContextMenu {
            position: event.position,
            items,
        });
        cx.notify();
    }

    fn toggle_selected(&mut self, asin: &str) {
        if !self.selected.remove(asin) {
            self.selected.insert(asin.to_owned());
        }
    }

    fn row_action(
        &self,
        t: &Theme,
        id: (&'static str, usize),
        info: &RowInfo,
        asin: String,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let label = info.label;
        let enabled = info.enabled;
        let base = if matches!(info.primary, Primary::Fetch) {
            primary(t, id, label, enabled)
        } else {
            button(t, id, label, enabled)
        };
        base.on_click(cx.listener(move |this, _, _, cx| {
            this.activate_book(&asin, cx);
            cx.notify();
        }))
    }

    fn list_row(
        &self,
        t: &Theme,
        position: usize,
        book: &LibraryBook,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let info = self.row_info(t, book);
        let asin = book.asin.clone();
        let (menu_asin, check_asin) = (asin.clone(), asin.clone());
        let focused = self.cursor == Some(position);
        let selected = self.selected.contains(&asin);
        let status_color = if info.error { t.error } else { t.muted };
        div()
            .h(px(LIST_ROW))
            .w_full()
            .flex_shrink_0()
            .pb_2()
            .child(
                div()
                    .id(("book", position))
                    .h_full()
                    .px_3()
                    .rounded_xl()
                    .border_1()
                    .border_color(rgb(if focused { t.selected } else { t.panel }))
                    .bg(rgb(if focused { t.panel_hover } else { t.panel }))
                    .flex()
                    .items_center()
                    .gap_3()
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            let items = this.library_menu(&menu_asin);
                            this.open_menu(event, items, cx);
                        }),
                    )
                    .child(
                        div()
                            .id(("select", position))
                            .cursor_pointer()
                            .child(checkbox(t, selected))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_selected(&check_asin);
                                cx.notify();
                            })),
                    )
                    .child(cover(book.cover.clone(), 64., t))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(div().truncate().child(book.title.clone()))
                            .child(
                                div()
                                    .truncate()
                                    .text_sm()
                                    .text_color(rgb(t.muted))
                                    .child(byline(book)),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(rgb(status_color))
                                    .child(info.status.clone().unwrap_or_else(|| meta_line(book))),
                            )
                            .when_some(info.progress, |element, fraction| {
                                element.child(progress_bar(t, fraction))
                            }),
                    )
                    .when_some(info.chip, |element, (text, color)| {
                        element.child(chip(t, text, color))
                    })
                    .child(self.row_action(t, ("action", position), &info, asin, cx)),
            )
            .into_any_element()
    }

    fn grid_card(
        &self,
        t: &Theme,
        position: usize,
        book: &LibraryBook,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let info = self.row_info(t, book);
        let asin = book.asin.clone();
        let (menu_asin, check_asin) = (asin.clone(), asin.clone());
        let focused = self.cursor == Some(position);
        let selected = self.selected.contains(&asin);
        div()
            .id(("card", position))
            .w(px(CARD_WIDTH))
            .h(px(GRID_ROW - GRID_GAP))
            .flex_shrink_0()
            .p_2()
            .rounded_xl()
            .border_1()
            .border_color(rgb(if focused { t.selected } else { t.panel }))
            .bg(rgb(if focused { t.panel_hover } else { t.panel }))
            .flex()
            .flex_col()
            .gap_1()
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    let items = this.library_menu(&menu_asin);
                    this.open_menu(event, items, cx);
                }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .id(("select", position))
                            .cursor_pointer()
                            .child(checkbox(t, selected))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_selected(&check_asin);
                                cx.notify();
                            })),
                    )
                    .when_some(info.chip, |element, (text, color)| {
                        element.child(chip(t, text, color))
                    }),
            )
            .child(cover(book.cover.clone(), CARD_WIDTH - 18., t))
            .child(div().truncate().text_sm().child(book.title.clone()))
            .child(
                div()
                    .truncate()
                    .text_xs()
                    .text_color(rgb(t.muted))
                    .child(book.authors.clone()),
            )
            .child(
                div()
                    .truncate()
                    .text_xs()
                    .text_color(rgb(if info.error { t.error } else { t.muted }))
                    .child(info.status.clone().unwrap_or_else(|| match book.bytes {
                        Some(bytes) => format!("{} · ≈ {}", book.length(), size_text(bytes)),
                        None => book.length(),
                    })),
            )
            .child(
                div().mt_auto().child(
                    self.row_action(t, ("action", position), &info, asin, cx)
                        .w_full()
                        .text_center(),
                ),
            )
            .into_any_element()
    }

    fn library_rows(
        &self,
        range: Range<usize>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let t = self.theme(window);
        let visible = self.visible();
        let books = self.library.as_deref().unwrap_or_default();
        if !self.settings.grid {
            return range
                .filter_map(|position| {
                    let book = books.get(*visible.get(position)?)?;
                    Some(self.list_row(&t, position, book, cx))
                })
                .collect();
        }
        let per_row = columns(window, true);
        range
            .map(|row| {
                let start = row * per_row;
                let end = (start + per_row).min(visible.len());
                div()
                    .h(px(GRID_ROW))
                    .w_full()
                    .flex()
                    .gap(px(GRID_GAP))
                    .children((start..end).map(|position| {
                        self.grid_card(&t, position, &books[visible[position]], cx)
                    }))
                    .into_any_element()
            })
            .collect()
    }

    fn library_pane(&mut self, window: &Window, t: &Theme, cx: &mut Context<Self>) -> Div {
        let pane = div().flex_1().min_h_0().flex().flex_col().gap_3();
        if !self.library_ready {
            return pane.child(
                div()
                    .p_4()
                    .rounded_xl()
                    .bg(rgb(t.panel))
                    .text_sm()
                    .child("Log out, then log in with Amazon again to browse your library. Pasted activation bytes cannot download books. You can still add AAX files in the Queue."),
            );
        }
        let loading = self.operation == Some(Operation::LoadingLibrary);
        let visible = self.visible();
        let total = self.library.as_ref().map_or(0, Vec::len);
        let books = self.library.as_deref().unwrap_or_default();
        let new_asins: Vec<String> = visible
            .iter()
            .map(|&i| &books[i])
            .filter(|b| b.output.is_none() && self.queue_index(&b.asin).is_none())
            .map(|b| b.asin.clone())
            .collect();
        let chosen: Vec<String> = visible
            .iter()
            .map(|&i| books[i].asin.clone())
            .filter(|asin| self.selected.contains(asin))
            .collect();
        let searching = self.search_focus.is_focused(window);
        let grid = self.settings.grid;
        let idle = self.operation != Some(Operation::LoggingOut) && self.dependencies;
        let per_row = columns(window, grid);
        let count = if grid {
            visible.len().div_ceil(per_row)
        } else {
            visible.len()
        };
        let (all, new) = (visible.len(), new_asins.len());
        let all_asins: Vec<String> = visible.iter().map(|&i| books[i].asin.clone()).collect();
        pane.child(
            div()
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .id("search")
                        .track_focus(&self.search_focus)
                        .on_key_down(cx.listener(Self::search_key))
                        .flex_1()
                        .min_w_0()
                        .px_3()
                        .py_2()
                        .rounded_lg()
                        .bg(rgb(t.panel))
                        .border_1()
                        .border_color(rgb(if searching { t.selected } else { t.border }))
                        .text_sm()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.search_focus.focus(window);
                            cx.notify();
                        }))
                        .child(if self.query.is_empty() {
                            div()
                                .text_color(rgb(t.muted))
                                .child("Search title, author, narrator or series  ⌘F")
                        } else {
                            div().child(format!(
                                "{}{}",
                                self.query,
                                if searching { "▏" } else { "" }
                            ))
                        }),
                )
                .child(segmented(
                    t,
                    "sort",
                    &[
                        (Sort::Recent, "Recent"),
                        (Sort::Title, "Title"),
                        (Sort::Author, "Author"),
                        (Sort::Length, "Length"),
                    ],
                    self.settings.sort,
                    cx,
                    |this, value| this.settings.sort = value,
                ))
                .child(segmented(
                    t,
                    "view",
                    &[(false, "List"), (true, "Grid")],
                    grid,
                    cx,
                    |this, value| this.settings.grid = value,
                )),
        )
        .child(
            div()
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(rgb(t.muted))
                        .child(if loading && self.library.is_none() {
                            "Loading your library…".to_owned()
                        } else if chosen.is_empty() {
                            format!("{all} of {total} books · saved to ~/Downloads, then converted")
                        } else {
                            format!("{} selected", chosen.len())
                        }),
                )
                .when(!chosen.is_empty(), |element| {
                    element
                        .child(
                            button(t, "clear-selection", "Clear", true).on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.selected.clear();
                                    cx.notify();
                                },
                            )),
                        )
                        .child(
                            primary(
                                t,
                                "download-selected",
                                format!("Download {}", chosen.len()),
                                idle,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.download_books(&chosen);
                                    this.selected.clear();
                                    cx.notify();
                                },
                            )),
                        )
                })
                .child(
                    button(t, "select-all", "Select all  ⌘A", all > 0).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.selected.extend(all_asins.iter().cloned());
                            cx.notify();
                        },
                    )),
                )
                .child(
                    button(
                        t,
                        "download-new",
                        format!("Download all new ({new})"),
                        idle && new > 0,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.download_books(&new_asins);
                        cx.notify();
                    })),
                )
                .child(
                    button(
                        t,
                        "refresh-library",
                        if loading { "Loading…" } else { "Refresh" },
                        self.worker.is_none(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.load_library();
                        cx.notify();
                    })),
                ),
        )
        .when_some(self.library_error.clone(), |element, error| {
            element.child(
                div()
                    .flex_shrink_0()
                    .p_3()
                    .rounded_xl()
                    .bg(rgb(t.panel))
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(rgb(t.error))
                            .child(error.clone()),
                    )
                    .child(
                        button(t, "copy-library-error", "Copy diagnostics", true).on_click(
                            cx.listener(move |this, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    this.diagnostics(&error),
                                ));
                            }),
                        ),
                    )
                    .child(
                        button(t, "retry-library", "Retry", self.worker.is_none()).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.load_library();
                                cx.notify();
                            }),
                        ),
                    ),
            )
        })
        .child(if count == 0 && !loading {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(t.muted))
                .child(if total == 0 && self.library_error.is_none() {
                    "No downloadable books found in this library."
                } else if self.library_error.is_some() {
                    ""
                } else {
                    "No books match your search."
                })
        } else {
            div().flex_1().min_h_0().child(
                uniform_list(
                    "library",
                    count,
                    cx.processor(|this, range: Range<usize>, window, cx| {
                        this.library_rows(range, window, cx)
                    }),
                )
                .track_scroll(self.list_scroll.clone())
                .size_full(),
            )
        })
    }

    fn clear_finished(&mut self) {
        if self.active.is_none() {
            self.books.retain(|book| book.state != State::Done);
            self.cursor = None;
        }
    }

    fn queue_pane(&mut self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let blocker = conversion_blocker(self.operation, self.connected, self.dependencies);
        let waiting = self
            .books
            .iter()
            .filter(|b| b.state == State::Queued)
            .count();
        let finished = self.books.iter().any(|b| b.state == State::Done);
        let can_edit = self.active.is_none() && self.operation != Some(Operation::LoggingOut);
        let started = self.operation == Some(Operation::Converting);
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(rgb(t.muted))
                            .child(format!("{} total · {waiting} waiting", self.books.len())),
                    )
                    .when(finished, |element| {
                        element.child(
                            button(t, "clear-finished", "Clear finished", can_edit).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.clear_finished();
                                    cx.notify();
                                }),
                            ),
                        )
                    })
                    .child(
                        button(
                            t,
                            "add",
                            "Add AAX files  ⌘O",
                            self.operation != Some(Operation::LoggingOut),
                        )
                        .on_click(
                            cx.listener(|this, _, window, cx| {
                                this.add_files(&AddFiles, window, cx)
                            }),
                        ),
                    )
                    .child(if started {
                        button(t, "stop", "Stop  Esc", !self.cancelling).on_click(
                            cx.listener(|this, _, window, cx| this.cancel(&Cancel, window, cx)),
                        )
                    } else {
                        primary(
                            t,
                            "convert",
                            if waiting == 0 {
                                "Convert  ⌘R".to_owned()
                            } else {
                                format!("Convert {waiting}  ⌘R")
                            },
                            blocker.is_none() && waiting > 0,
                        )
                        .on_click(
                            cx.listener(|this, _, window, cx| this.convert(&Convert, window, cx)),
                        )
                    }),
            )
            .when_some(
                blocker.filter(|_| waiting > 0 && !started),
                |element, reason| {
                    element.child(
                        div()
                            .flex_shrink_0()
                            .text_sm()
                            .text_color(rgb(t.warn))
                            .child(reason),
                    )
                },
            )
            .child(
                div()
                    .id("queue")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .when(self.books.is_empty(), |element| {
                        element.child(
                            div()
                                .h_40()
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_xl()
                                .border_1()
                                .border_color(rgb(t.border))
                                .text_color(rgb(t.muted))
                                .child("Drop AAX files here, or pick books in the Library."),
                        )
                    })
                    .children(
                        self.books
                            .iter()
                            .enumerate()
                            .map(|(index, book)| self.queue_row(t, index, book, can_edit, cx)),
                    ),
            )
    }

    fn queue_row(
        &self,
        t: &Theme,
        index: usize,
        book: &Book,
        can_edit: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let retry = matches!(book.state, State::Failed | State::Cancelled);
        let failed = book.state == State::Failed;
        let focused = self.cursor == Some(index);
        let artwork = book
            .asin
            .as_ref()
            .and_then(|asin| {
                self.library
                    .iter()
                    .flatten()
                    .find(|entry| &entry.asin == asin)
            })
            .and_then(|entry| entry.cover.clone());
        let output = book.output.clone();
        let detail = book.detail.clone();
        let problem = self.diagnostics(&detail);
        div()
            .id(("queued", index))
            .flex_shrink_0()
            .p_3()
            .rounded_xl()
            .border_1()
            .border_color(rgb(if focused { t.selected } else { t.panel }))
            .bg(rgb(if focused { t.panel_hover } else { t.panel }))
            .flex()
            .flex_col()
            .gap_2()
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    let items = this.queue_menu(index);
                    this.open_menu(event, items, cx);
                }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .when(artwork.is_some(), |element| {
                        element.child(cover(artwork, 48., t))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(book.title.clone()),
                    )
                    .when_some(output, |element, path| {
                        element.child(
                            button(t, ("finder", index), "Show in Finder", true)
                                .on_click(move |_, _, cx| cx.reveal_path(&path)),
                        )
                    })
                    .when(failed, |element| {
                        element.child(
                            button(t, ("diagnostics", index), "Copy diagnostics", true).on_click(
                                move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        problem.clone(),
                                    ))
                                },
                            ),
                        )
                    })
                    .when(retry, |element| {
                        element.child(
                            button(
                                t,
                                ("retry", index),
                                if failed { "Retry" } else { "Queue again" },
                                can_edit || self.worker.is_none(),
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.retry(index);
                                    cx.notify();
                                },
                            )),
                        )
                    })
                    .when(book.state != State::Running, |element| {
                        element.child(button(t, ("remove", index), "Remove", can_edit).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.remove(index);
                                cx.notify();
                            }),
                        ))
                    }),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(if retry { t.error } else { t.muted }))
                    .child(detail),
            )
            .when(
                matches!(book.state, State::Running | State::Done),
                |element| element.child(progress_bar(t, book.progress)),
            )
            .into_any_element()
    }

    fn settings_pane(&mut self, t: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let free = |dir: &Path| {
            settings::free_bytes(dir).map_or("unknown".to_owned(), |bytes| {
                format!("{} free", size_text(bytes))
            })
        };
        let dirs = self.settings.work_dirs();
        let downloads = format!("~/Downloads · {}", free(&dirs[0]));
        let folder = match &self.settings.output_dir {
            Some(dir) => format!("{} · {}", dir.display(), free(dir)),
            None => format!(
                "Next to the downloaded file, in ~/Downloads · {}",
                free(&dirs[0])
            ),
        };
        let choose = button(t, "choose-folder", "Choose…", true)
            .on_click(cx.listener(|this, _, _, cx| this.choose_output_folder(cx)));
        let reset = button(
            t,
            "reset-folder",
            "Default",
            self.settings.output_dir.is_some(),
        )
        .on_click(cx.listener(|this, _, _, cx| {
            this.settings.output_dir = None;
            this.save_settings();
            cx.notify();
        }));
        let section = |title: &'static str| {
            div()
                .pt_4()
                .text_xs()
                .text_color(rgb(t.muted))
                .child(title.to_uppercase())
        };
        div()
            .id("settings")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .max_w(px(720.))
            .child(section("Output"))
            .child(setting(
                t,
                "Format",
                if self.settings.format == Format::M4b {
                    "One M4B per book with chapter markers. Works in Apple Books, Plex, Audiobookshelf and most players.".into()
                } else {
                    "One M4A file per chapter, in a folder per book.".into()
                },
                segmented(
                    t,
                    "format",
                    &[(Format::Chapters, "Chapter files"), (Format::M4b, "Single M4B")],
                    self.settings.format,
                    cx,
                    |this, value| this.settings.format = value,
                ),
            ))
            .child(setting(
                t,
                "Folder layout",
                if self.settings.layout == Layout::Author {
                    "Author / Series / Book 3 - Title, which media servers sort automatically.".into()
                } else {
                    "Each book directly in the output folder.".into()
                },
                segmented(
                    t,
                    "layout",
                    &[(Layout::Flat, "Flat"), (Layout::Author, "Author / Series")],
                    self.settings.layout,
                    cx,
                    |this, value| this.settings.layout = value,
                ),
            ))
            .child(setting(
                t,
                "Output folder",
                folder,
                div().flex().gap_2().child(choose).child(reset),
            ))
            .child(section("Disk space"))
            .child(setting(
                t,
                "Move the original to the Trash after converting",
                format!("Off by default. A book takes about as much space as its download. Downloads: {downloads}"),
                toggle(t, "trash-source", self.settings.trash_source, cx, |this| {
                    this.settings.trash_source = !this.settings.trash_source
                }),
            ))
            .child(section("Appearance"))
            .child(setting(
                t,
                "Theme",
                "Follows macOS unless you choose one.".into(),
                segmented(
                    t,
                    "appearance",
                    &[
                        (Appearance::System, "System"),
                        (Appearance::Light, "Light"),
                        (Appearance::Dark, "Dark"),
                    ],
                    self.settings.appearance,
                    cx,
                    |this, value| this.settings.appearance = value,
                ),
            ))
            .child(section("Updates"))
            .child(setting(
                t,
                "Check for new versions",
                "Asks GitHub for the latest release when Earshelf starts. Nothing about you is sent.".into(),
                toggle(t, "check-updates", self.settings.check_updates, cx, |this| {
                    this.settings.check_updates = !this.settings.check_updates
                }),
            ))
            .child(section("About"))
            .child(
                div()
                    .py_3()
                    .text_sm()
                    .text_color(rgb(t.muted))
                    .child(format!(
                        "Earshelf {} · for audiobooks you own only · not affiliated with Audible or Amazon.",
                        env!("CARGO_PKG_VERSION")
                    )),
            )
            .child(
                div().flex().gap_2().pb_4().child(
                    button(t, "open-issues", "Report a problem", true)
                        .on_click(|_, _, cx| cx.open_url("https://github.com/Mokto/earshelf/issues/new?template=bug_report.yml")),
                ),
            )
    }

    fn nav_item(
        &self,
        t: &Theme,
        id: &'static str,
        label: &'static str,
        section: Section,
        badge: usize,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let current = self.section == section;
        div()
            .id(id)
            .px_3()
            .py_2()
            .rounded_lg()
            .flex()
            .items_center()
            .justify_between()
            .cursor_pointer()
            .text_color(rgb(t.text))
            .when(current, |element| element.bg(rgb(t.selected)))
            .when(!current, |element| {
                element.hover(|style| style.bg(rgb(t.button)))
            })
            .child(label)
            .when(badge > 0, |element| {
                element.child(
                    div()
                        .px_2()
                        .rounded_full()
                        .bg(rgb(t.accent))
                        .text_xs()
                        .text_color(rgb(WHITE))
                        .child(badge.to_string()),
                )
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.show(section);
                cx.notify();
            }))
    }

    pub fn show(&mut self, section: Section) {
        if self.connected {
            self.section = section;
            self.cursor = None;
            self.menu = None;
        }
    }

    fn sidebar(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let pending = self
            .books
            .iter()
            .filter(|book| matches!(book.state, State::Queued | State::Running))
            .count();
        let failed = self
            .books
            .iter()
            .filter(|b| b.state == State::Failed)
            .count();
        let busy = self.worker.is_some();
        let flag = match self.settings.locale.as_str() {
            "uk" => "🇬🇧 UK",
            "us" => "🇺🇸 US",
            "de" => "🇩🇪 DE",
            "au" => "🇦🇺 AU",
            "ca" => "🇨🇦 CA",
            _ => "🇫🇷 FR",
        };
        div()
            .w(px(SIDEBAR))
            .h_full()
            .flex_shrink_0()
            .bg(rgb(t.sidebar))
            .p_3()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .px_2()
                    .pb_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(logo(36.))
                    .child(
                        div()
                            .text_xl()
                            .font_weight(gpui::FontWeight::BOLD)
                            .child("Earshelf"),
                    ),
            )
            .child(self.nav_item(t, "nav-library", "Library", Section::Library, 0, cx))
            .child(self.nav_item(t, "nav-queue", "Queue", Section::Queue, pending, cx))
            .child(self.nav_item(t, "nav-settings", "Settings", Section::Settings, 0, cx))
            .child(div().flex_1())
            .when(failed > 0, |element| {
                element.child(
                    div()
                        .px_3()
                        .py_1()
                        .text_xs()
                        .text_color(rgb(t.error))
                        .child(format!("{failed} failed · see Queue")),
                )
            })
            .when_some(
                self.active.and_then(|i| self.books.get(i)),
                |element, book| {
                    element.child(
                        div()
                            .p_3()
                            .rounded_lg()
                            .bg(rgb(t.panel))
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(div().truncate().text_sm().child(book.title.clone()))
                            .child(progress_bar(t, book.progress))
                            .child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(rgb(t.muted))
                                    .child(book.detail.clone()),
                            )
                            .child(
                                button(t, "sidebar-stop", "Stop", !self.cancelling).on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.cancel(&Cancel, window, cx)
                                    }),
                                ),
                            ),
                    )
                },
            )
            .child(
                div()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(t.muted))
                            .child(format!("{flag} · signed in")),
                    )
                    .child(
                        button(
                            t,
                            "logout",
                            if self.operation == Some(Operation::LoggingOut) {
                                "Logging out…"
                            } else {
                                "Log out"
                            },
                            !busy,
                        )
                        .on_click(
                            cx.listener(|this, _, window, cx| this.logout(&Logout, window, cx)),
                        ),
                    ),
            )
    }

    fn login_view(&self, t: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let busy = self.worker.is_some();
        let signing_in = self.operation.is_some_and(Operation::is_login);
        let waiting_for_url = self.operation == Some(Operation::LoginWaiting) && !self.cancelling;
        div()
            .id("login-view")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_5()
            .flex()
            .flex_col()
            .items_center()
            .child(
                div()
                    .w_full()
                    .max_w(px(640.))
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .my_auto()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(logo(72.))
                            .child(div().text_2xl().font_weight(gpui::FontWeight::BOLD).child("Earshelf"))
                            .child(
                                div()
                                    .text_color(rgb(t.muted))
                                    .child("Keep the Audible audiobooks you own as chapter files or one M4B, with no loss in quality."),
                            ),
                    )
                    .when(!self.settings.onboarded && !signing_in, |element| {
                        element.child(
                            div()
                                .p_4()
                                .rounded_xl()
                                .bg(rgb(t.panel))
                                .border_1()
                                .border_color(rgb(t.border))
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(div().font_weight(gpui::FontWeight::BOLD).child("Before you start"))
                                .child(div().text_sm().child("• Earshelf is for books you own. Don't share or distribute the files."))
                                .child(div().text_sm().child("• You sign in on Amazon's own page in a private window. Your password is never saved."))
                                .child(div().text_sm().child("• Saved on this Mac: a device key in your Keychain and your activation bytes in ~/.audible."))
                                .child(div().text_sm().child("• Downloads go to ~/Downloads; your originals are never changed or deleted unless you turn that on."))
                                .child(
                                    div().flex().child(
                                        button(t, "welcome-ok", "Got it", true).on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.settings.onboarded = true;
                                                this.save_settings();
                                                cx.notify();
                                            }),
                                        ),
                                    ),
                                ),
                        )
                    })
                    .when(!signing_in, |element| {
                        element.child(
                            div()
                                .p_5()
                                .rounded_xl()
                                .bg(rgb(t.panel))
                                .flex()
                                .flex_col()
                                .gap_4()
                                .child(div().text_sm().child("Choose the marketplace where your Audible library lives."))
                                .child(div().flex().flex_wrap().gap_2().children(
                                    [("uk", "🇬🇧 UK"), ("us", "🇺🇸 US"), ("de", "🇩🇪 DE"), ("au", "🇦🇺 AU"), ("ca", "🇨🇦 CA"), ("fr", "🇫🇷 FR")]
                                        .into_iter()
                                        .map(|(code, label)| {
                                            button(t, code, label, !busy)
                                                .bg(rgb(if self.settings.locale == code { t.selected } else { t.button }))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    if !this.connected && this.worker.is_none() {
                                                        this.settings.locale = code.into();
                                                        this.save_settings();
                                                        cx.notify();
                                                    }
                                                }))
                                        }),
                                ))
                                .child(
                                    primary(t, "connect", "Log in with Amazon  ⌘L", !busy)
                                        .on_click(cx.listener(|this, _, window, cx| this.connect(&Connect, window, cx))),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(rgb(t.muted))
                                        .child("The sign-in window captures the login result automatically. 1Password autofill is optional."),
                                )
                                .child(
                                    div()
                                        .id("advanced")
                                        .text_sm()
                                        .text_color(rgb(t.muted))
                                        .cursor_pointer()
                                        .child(if self.advanced { "▾ Advanced" } else { "▸ Advanced" })
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.advanced = !this.advanced;
                                            cx.notify();
                                        })),
                                )
                                .when(self.advanced, |element| {
                                    element
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(rgb(t.muted))
                                                .child("Already have your activation bytes? You can convert files you already downloaded, but you can't browse or download from your library."),
                                        )
                                        .child(
                                            div().flex().child(
                                                button(t, "paste-bytes", "Use copied activation bytes  ⇧⌘V", !busy)
                                                    .on_click(cx.listener(|this, _, window, cx| this.paste_bytes(&PasteBytes, window, cx))),
                                            ),
                                        )
                                }),
                        )
                    })
                    .when(signing_in, |element| {
                        element.child(
                            div()
                                .p_4()
                                .rounded_xl()
                                .bg(rgb(t.status))
                                .flex()
                                .flex_col()
                                .gap_3()
                                .child(div().font_weight(gpui::FontWeight::BOLD).child(
                                    if self.cancelling {
                                        "Cancelling Audible sign-in…"
                                    } else if self.operation == Some(Operation::LoginFinishing) {
                                        "Fetching your activation bytes…"
                                    } else {
                                        "Sign in with Amazon"
                                    },
                                ))
                                .child(div().text_sm().child(
                                    if self.operation == Some(Operation::LoginFinishing) {
                                        "The login result was received. No further action is needed."
                                    } else {
                                        "Complete Amazon sign-in, MFA, or CAPTCHA in the sign-in window. Earshelf captures the result automatically and never opens an external browser."
                                    },
                                ))
                                .child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .gap_2()
                                        .child(
                                            button(t, "show-login", "Show sign-in window", waiting_for_url).on_click(
                                                cx.listener(|this, _, _, cx| {
                                                    if this.operation == Some(Operation::LoginWaiting) && !this.cancelling {
                                                        this.open_login_window();
                                                        cx.notify();
                                                    }
                                                }),
                                            ),
                                        )
                                        .child(
                                            button(t, "cancel-login", "Cancel sign-in  Esc", !self.cancelling)
                                                .on_click(cx.listener(|this, _, window, cx| this.cancel(&Cancel, window, cx))),
                                        ),
                                ),
                        )
                    })
                    .child(
                        div()
                            .px_4()
                            .py_3()
                            .rounded_lg()
                            .bg(rgb(t.status))
                            .text_sm()
                            .text_color(rgb(t.status_text))
                            .child(self.message.clone()),
                    ),
            )
    }

    fn menu_overlay(&self, t: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.menu.clone()?;
        let hover = t.button_hover;
        Some(
            deferred(
                anchored().position(menu.position).snap_to_window().child(
                    div()
                        .id("context-menu")
                        .w(px(200.))
                        .p_1()
                        .rounded_lg()
                        .bg(rgb(t.panel))
                        .border_1()
                        .border_color(rgb(t.border))
                        .shadow_lg()
                        .flex()
                        .flex_col()
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                            this.menu = None;
                            cx.notify();
                        }))
                        .children(menu.items.into_iter().enumerate().map(
                            |(index, (label, action))| {
                                div()
                                    .id(("menu-item", index))
                                    .px_3()
                                    .py(px(5.))
                                    .rounded_md()
                                    .text_sm()
                                    .cursor_pointer()
                                    .hover(|style| style.bg(rgb(hover)))
                                    .child(label)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.run_menu(action.clone(), cx)
                                    }))
                            },
                        )),
                ),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    fn main_pane(&mut self, window: &Window, t: &Theme, cx: &mut Context<Self>) -> Div {
        let status_color = if self.operation_failed {
            t.error
        } else {
            t.status_text
        };
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .p_5()
            .gap_3()
            .when_some(self.update.clone(), |element, (version, url)| {
                element.child(
                    div()
                        .flex_shrink_0()
                        .px_4()
                        .py_2()
                        .rounded_lg()
                        .bg(rgb(t.status))
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .flex_1()
                                .text_sm()
                                .text_color(rgb(t.status_text))
                                .child(format!("Earshelf {version} is available.")),
                        )
                        .child(
                            button(t, "view-update", "View release", true)
                                .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url))),
                        )
                        .child(
                            button(t, "dismiss-update", "Dismiss", true).on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.update = None;
                                    cx.notify();
                                },
                            )),
                        ),
                )
            })
            .child(match self.section {
                Section::Library => self.library_pane(window, t, cx).into_any_element(),
                Section::Queue => self.queue_pane(t, cx).into_any_element(),
                Section::Settings => self.settings_pane(t, cx).into_any_element(),
            })
            .child(
                div()
                    .flex_shrink_0()
                    .px_4()
                    .py_2()
                    .rounded_lg()
                    .bg(rgb(t.status))
                    .text_xs()
                    .text_color(rgb(status_color))
                    .child(self.message.clone()),
            )
    }
}

impl Render for Earshelf {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme(window);
        let mut root = div()
            .id("app")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .bg(rgb(t.bg))
            .text_color(rgb(t.text))
            .font_family(".AppleSystemUIFont")
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::quit))
            .on_key_down(cx.listener(Self::key_down))
            .on_drop(
                cx.listener(|this, paths: &ExternalPaths, _, cx| this.add_paths(paths.paths(), cx)),
            );
        if self.connected {
            root = root
                .on_action(cx.listener(Self::add_files))
                .on_action(cx.listener(Self::convert))
                .on_action(cx.listener(Self::logout))
                .on_action(cx.listener(|this, _: &ShowLibrary, _, cx| {
                    this.show(Section::Library);
                    cx.notify();
                }))
                .on_action(cx.listener(|this, _: &ShowQueue, _, cx| {
                    this.show(Section::Queue);
                    cx.notify();
                }))
                .on_action(cx.listener(|this, _: &ShowSettings, _, cx| {
                    this.show(Section::Settings);
                    cx.notify();
                }))
                .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                    this.show(Section::Library);
                    this.search_focus.focus(window);
                    cx.notify();
                }))
                .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
                    if this.section == Section::Library {
                        let all: Vec<String> = this
                            .visible()
                            .into_iter()
                            .map(|i| this.library.as_ref().unwrap()[i].asin.clone())
                            .collect();
                        this.selected.extend(all);
                        cx.notify();
                    }
                }))
                .child(self.sidebar(&t, cx))
                .child(self.main_pane(window, &t, cx));
        } else {
            root = root
                .on_action(cx.listener(Self::connect))
                .on_action(cx.listener(Self::paste_bytes))
                .child(self.login_view(&t, cx));
        }
        root.children(self.menu_overlay(&t, cx))
    }
}
