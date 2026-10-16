//! One thread per connection (and per database in PostgreSQL). It receives tasks that are
//! closures over the driver, so the UI never blocks.

use std::any::Any;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

use eframe::egui;
use lumedb_core::{connect, Canceller, ConnectionConfig, Driver};

pub type TaskOutput = Box<dyn Any + Send>;
pub type TaskResult = Result<TaskOutput, String>;
pub type TaskFn = Box<dyn FnOnce(&mut dyn Driver, &Progress) -> anyhow::Result<TaskOutput> + Send>;

pub enum Event {
    Connected { cancel: Option<Canceller> },
    ConnectFailed(String),
    Progress { id: u64, message: String },
    Done { id: u64, result: TaskResult },
}

/// Lets a task report its progress.
#[derive(Clone)]
pub struct Progress {
    tx: Sender<Event>,
    id: u64,
    ctx: egui::Context,
}

impl Progress {
    pub fn report(&self, message: impl Into<String>) {
        let _ = self.tx.send(Event::Progress { id: self.id, message: message.into() });
        self.ctx.request_repaint();
    }
}

pub struct Worker {
    tx: Sender<(u64, TaskFn)>,
    pub rx: Receiver<Event>,
}

impl Worker {
    pub fn spawn(cfg: ConnectionConfig, ctx: egui::Context) -> Self {
        let (task_tx, task_rx) = channel::<(u64, TaskFn)>();
        let (ev_tx, ev_rx) = channel::<Event>();
        thread::spawn(move || {
            let send = |ev| {
                let _ = ev_tx.send(ev);
                ctx.request_repaint();
            };
            let mut driver = match connect(&cfg) {
                Ok(d) => d,
                Err(e) => return send(Event::ConnectFailed(format!("{e:#}"))),
            };
            send(Event::Connected { cancel: driver.canceller() });
            for (id, task) in task_rx {
                let progress = Progress { tx: ev_tx.clone(), id, ctx: ctx.clone() };
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| task(driver.as_mut(), &progress)))
                    .unwrap_or_else(|_| Err(anyhow::anyhow!(lumedb_core::tr("internal error while running the task"))));
                send(Event::Done { id, result: result.map_err(|e| format!("{e:#}")) });
            }
        });
        Self { tx: task_tx, rx: ev_rx }
    }

    pub fn send(&self, id: u64, task: TaskFn) {
        let _ = self.tx.send((id, task));
    }
}

/// Standalone task on its own thread (transfers between connections, imports…).
pub struct Background<T> {
    rx: Receiver<BgEvent<T>>,
    pub progress: String,
    pub result: Option<Result<T, String>>,
}

enum BgEvent<T> {
    Progress(String),
    Done(Result<T, String>),
}

impl<T: Send + 'static> Background<T> {
    pub fn spawn(ctx: &egui::Context, f: impl FnOnce(&dyn Fn(String)) -> anyhow::Result<T> + Send + 'static) -> Self {
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let report = |m: String| {
                let _ = tx.send(BgEvent::Progress(m));
                ctx.request_repaint();
            };
            let r = f(&report).map_err(|e| format!("{e:#}"));
            let _ = tx.send(BgEvent::Done(r));
            ctx.request_repaint();
        });
        Self { rx, progress: String::new(), result: None }
    }

    /// Returns `true` when the task has finished.
    pub fn poll(&mut self) -> bool {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                BgEvent::Progress(p) => self.progress = p,
                BgEvent::Done(r) => self.result = Some(r),
            }
        }
        self.result.is_some()
    }
}
