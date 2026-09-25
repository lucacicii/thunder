use crossterm::event::{self as ct_event, Event as CtEvent, KeyEvent, MouseEvent};
use std::time::Duration;
use thunder_agent_loop::types::event::ObservedEvent;
use tokio::sync::mpsc;

#[derive(Debug)]
pub enum AppEvent {
    /// Keyboard input from terminal
    Key(KeyEvent),
    /// Mouse input
    Mouse(MouseEvent),
    /// Terminal resize
    Resize(u16, u16),
    /// Periodic tick for animations and status refresh
    Tick,
    /// Live agent event stream from single agent loop or orchestra
    Agent(ObservedEvent),
    /// Agent run finished with final status
    AgentFinished {
        agent_id: String,
        success: bool,
        final_text: Option<String>,
        authoritative_messages: Option<Vec<thunder_agent_loop::types::message::ChatMessage>>,
    },
    /// Request to load a specific session by ID
    LoadSession(String),
    /// Notification that a session was deleted
    SessionDeleted(String),
    /// Open an interactive picker after an async scan completes
    OpenPicker {
        kind: crate::picker::PickerKind,
        title: String,
        items: Vec<crate::picker::PickerItem>,
        empty_message: Option<String>,
    },
}

pub struct EventHandler {
    rx: mpsc::UnboundedReceiver<AppEvent>,
    tx: mpsc::UnboundedSender<AppEvent>,
}

impl EventHandler {
    pub fn new(tick_rate: Duration) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let event_tx = tx.clone();

        // Terminal event listener task
        tokio::spawn(async move {
            loop {
                if ct_event::poll(Duration::from_millis(20)).unwrap_or(false) {
                    match ct_event::read() {
                        Ok(CtEvent::Key(key)) => {
                            if event_tx.send(AppEvent::Key(key)).is_err() {
                                break;
                            }
                        }
                        Ok(CtEvent::Mouse(mouse)) => {
                            let _ = event_tx.send(AppEvent::Mouse(mouse));
                        }
                        Ok(CtEvent::Resize(w, h)) => {
                            let _ = event_tx.send(AppEvent::Resize(w, h));
                        }
                        _ => {}
                    }
                }

                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });

        // Periodic tick generator task
        let tick_tx = tx.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tick_rate);
            loop {
                interval.tick().await;
                if tick_tx.send(AppEvent::Tick).is_err() {
                    break;
                }
            }
        });

        Self { rx, tx }
    }

    pub fn sender(&self) -> mpsc::UnboundedSender<AppEvent> {
        self.tx.clone()
    }

    pub async fn next(&mut self) -> Option<AppEvent> {
        self.rx.recv().await
    }

    pub fn try_next(&mut self) -> Result<AppEvent, mpsc::error::TryRecvError> {
        self.rx.try_recv()
    }
}
