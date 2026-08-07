//! iced-based source picker.
//!
//! `iced::daemon` lets us run an app with no initial window. We open a window
//! when a `PickRequest` arrives over the cross-thread channel and close it
//! once the user clicks a choice (or cancels). cosmic-text under the hood
//! handles font fallback, so CJK strings render without any font setup.
//!
//! Threading model:
//! - main thread runs iced (winit event loop)
//! - the D-Bus / tokio worker thread sends `PickRequest`s via an mpsc channel
//! - the receiver is parked in a `OnceLock` and pulled out by the iced
//!   subscription on first poll

use std::sync::{Mutex, OnceLock};

use iced::widget::{button, column, container, image, row, scrollable, space, text};
use iced::window;
use iced::{Element, Length, Subscription, Task};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::i18n;
use crate::sources::{SourceInfo, SourceKind, ThumbnailUpdate};

#[derive(Debug, Clone)]
pub enum PickResult {
    Source(SourceInfo),
    Cancelled,
}

pub struct PickRequest {
    pub sources: Vec<SourceInfo>,
    pub responder: oneshot::Sender<PickResult>,
}

/// Handle the D-Bus side uses to request a pick. Cheap to clone.
#[derive(Clone)]
pub struct PickerHandle {
    tx: mpsc::UnboundedSender<PickRequest>,
}

impl PickerHandle {
    pub async fn pick(&self, sources: Vec<SourceInfo>) -> PickResult {
        let (responder, rx) = oneshot::channel();
        if self.tx.send(PickRequest { sources, responder }).is_err() {
            tracing::error!("picker thread is gone");
            return PickResult::Cancelled;
        }
        rx.await.unwrap_or(PickResult::Cancelled)
    }
}

type ReceiverSlot = Mutex<Option<mpsc::UnboundedReceiver<PickRequest>>>;
static PICKER_RX: OnceLock<ReceiverSlot> = OnceLock::new();

type ThumbnailRxSlot = Mutex<Option<mpsc::UnboundedReceiver<ThumbnailUpdate>>>;
static THUMBNAIL_RX: OnceLock<ThumbnailRxSlot> = OnceLock::new();

pub fn setup() -> (PickerHandle, mpsc::UnboundedSender<ThumbnailUpdate>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let _ = PICKER_RX.set(Mutex::new(Some(rx)));
    let (thumb_tx, thumb_rx) = mpsc::unbounded_channel();
    let _ = THUMBNAIL_RX.set(Mutex::new(Some(thumb_rx)));
    (PickerHandle { tx }, thumb_tx)
}

pub fn run_on_main_thread() -> iced::Result {
    iced::daemon(|| (State::default(), Task::none()), update, view)
        .title(|_state: &State, _id: window::Id| i18n::t("picker.window_title"))
        .subscription(subscription)
        .run()
}

#[derive(Default)]
struct State {
    active: Option<Active>,
    window_id: Option<window::Id>,
}

struct Active {
    sources: Vec<SourceInfo>,
    responder: Option<oneshot::Sender<PickResult>>,
    tab: Tab,
}

#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
enum Tab {
    #[default]
    FullScreen,
    Window,
}

#[derive(Debug, Clone)]
enum Message {
    RequestArrived(RequestArrived),
    WindowOpened(window::Id),
    WindowClosed(window::Id),
    SourceClicked(usize),
    Cancelled,
    TabSelected(Tab),
    ThumbnailUpdate(ThumbnailUpdate),
}

/// `(picked sources, responder)` handed off to the picker's request handler.
type PickHandoff = (Vec<SourceInfo>, oneshot::Sender<PickResult>);

/// Boxed handoff for the responder so Message can stay `Clone`.
#[derive(Debug, Clone)]
struct RequestArrived(std::sync::Arc<Mutex<Option<PickHandoff>>>);

fn update(state: &mut State, message: Message) -> Task<Message> {
    match message {
        Message::RequestArrived(arrived) => {
            let Some((sources, responder)) = arrived.0.lock().unwrap().take() else {
                return Task::none();
            };
            if let Some(mut prev) = state.active.take()
                && let Some(r) = prev.responder.take()
            {
                let _ = r.send(PickResult::Cancelled);
            }
            state.active = Some(Active {
                sources,
                responder: Some(responder),
                tab: Tab::default(),
            });
            if state.window_id.is_some() {
                Task::none()
            } else {
                let (id, open_task) = window::open(window::Settings {
                    size: iced::Size::new(560.0, 520.0),
                    min_size: Some(iced::Size::new(360.0, 280.0)),
                    ..Default::default()
                });
                state.window_id = Some(id);
                open_task.map(Message::WindowOpened)
            }
        }
        Message::WindowOpened(_id) => Task::none(),
        Message::WindowClosed(id) => {
            if state.window_id == Some(id) {
                state.window_id = None;
            }
            if let Some(mut active) = state.active.take()
                && let Some(r) = active.responder.take()
            {
                let _ = r.send(PickResult::Cancelled);
            }
            Task::none()
        }
        Message::SourceClicked(index) => finish(state, |sources| {
            sources.get(index).cloned().map(PickResult::Source)
        }),
        Message::Cancelled => finish(state, |_| Some(PickResult::Cancelled)),
        Message::TabSelected(tab) => {
            if let Some(active) = state.active.as_mut() {
                active.tab = tab;
            }
            Task::none()
        }
        Message::ThumbnailUpdate(update) => {
            if let Some(active) = state.active.as_mut() {
                for src in &mut active.sources {
                    if src.id() == update.source_id {
                        src.thumbnail = Some(update.handle.clone());
                        break;
                    }
                }
            }
            Task::none()
        }
    }
}

fn finish<F>(state: &mut State, choose: F) -> Task<Message>
where
    F: FnOnce(&[SourceInfo]) -> Option<PickResult>,
{
    let Some(mut active) = state.active.take() else {
        return Task::none();
    };
    let result = choose(&active.sources).unwrap_or(PickResult::Cancelled);
    if let Some(r) = active.responder.take() {
        let _ = r.send(result);
    }
    if let Some(id) = state.window_id.take() {
        window::close(id)
    } else {
        Task::none()
    }
}

fn view(state: &State, _id: window::Id) -> Element<'_, Message> {
    let Some(active) = state.active.as_ref() else {
        return container(text(i18n::t("picker.idle"))).padding(16).into();
    };

    // Counts per tab, used to label and empty-state.
    let (n_outputs, n_windows) =
        active
            .sources
            .iter()
            .fold((0usize, 0usize), |acc, s| match s.kind {
                SourceKind::Output(_) => (acc.0 + 1, acc.1),
                SourceKind::Toplevel(_) => (acc.0, acc.1 + 1),
            });

    let tabs = row![
        tab_button(
            i18n::t_args(
                "picker.tab_fullscreen",
                &[("count", &n_outputs.to_string())]
            ),
            active.tab == Tab::FullScreen,
            Tab::FullScreen
        ),
        tab_button(
            i18n::t_args("picker.tab_window", &[("count", &n_windows.to_string())]),
            active.tab == Tab::Window,
            Tab::Window
        ),
    ]
    .spacing(8);

    if active.sources.is_empty() {
        let no_sources = container(text(i18n::t("picker.no_sources"))).padding(16);
        return container(
            column![text(i18n::t("picker.heading")).size(18), tabs, no_sources].spacing(12),
        )
        .padding(16)
        .into();
    }

    let mut list = column![].spacing(6);
    let mut shown_any = false;
    for (i, src) in active.sources.iter().enumerate() {
        let in_tab = matches!(
            (active.tab, &src.kind),
            (Tab::FullScreen, SourceKind::Output(_)) | (Tab::Window, SourceKind::Toplevel(_))
        );
        if !in_tab {
            continue;
        }
        shown_any = true;

        let header = src.label();
        let detail = src.detail();
        let text_column: Element<'_, Message> =
            column![text(header).size(14), text(detail).size(12)]
                .spacing(2)
                .into();

        let row_content: Element<'_, Message> = if let Some(handle) = src.thumbnail.as_ref() {
            row![
                image(handle.clone())
                    .width(Length::Fixed(120.0))
                    .height(Length::Fixed(75.0))
                    .content_fit(iced::ContentFit::Contain),
                text_column,
            ]
            .spacing(12)
            .align_y(iced::Center)
            .into()
        } else {
            text_column
        };

        list = list.push(
            button(row_content)
                .width(Length::Fill)
                .padding(10)
                .on_press(Message::SourceClicked(i)),
        );
    }
    if !shown_any {
        let key = match active.tab {
            Tab::FullScreen => "picker.no_outputs",
            Tab::Window => "picker.no_windows",
        };
        list = list.push(text(i18n::t(key)));
    }

    let body = scrollable(list).height(Length::Fill);

    let footer = row![
        space::horizontal(),
        button(text(i18n::t("picker.cancel"))).on_press(Message::Cancelled),
    ];

    container(column![text(i18n::t("picker.heading")).size(18), tabs, body, footer,].spacing(12))
        .padding(16)
        .into()
}

fn tab_button(label: String, selected: bool, kind: Tab) -> Element<'static, Message> {
    let btn = button(text(label));
    let btn = if selected {
        btn.style(button::primary)
    } else {
        btn.style(button::secondary)
    };
    btn.on_press(Message::TabSelected(kind)).into()
}

fn subscription(_state: &State) -> Subscription<Message> {
    let request_stream = Subscription::run(|| {
        let rx = PICKER_RX
            .get()
            .expect("picker::setup() not called before run_on_main_thread")
            .lock()
            .unwrap()
            .take()
            .expect("picker subscription started twice");
        UnboundedReceiverStream::new(rx).map(|req| {
            Message::RequestArrived(RequestArrived(std::sync::Arc::new(Mutex::new(Some((
                req.sources,
                req.responder,
            ))))))
        })
    });

    let thumbnail_stream = Subscription::run(|| {
        let rx = THUMBNAIL_RX
            .get()
            .expect("picker::setup() not called before run_on_main_thread")
            .lock()
            .unwrap()
            .take()
            .expect("thumbnail subscription started twice");
        UnboundedReceiverStream::new(rx).map(Message::ThumbnailUpdate)
    });

    let close_events = window::close_events().map(Message::WindowClosed);

    Subscription::batch([request_stream, thumbnail_stream, close_events])
}
