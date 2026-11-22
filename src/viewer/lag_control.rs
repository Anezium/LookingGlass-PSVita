//! Host controls for falsifying the lag-one overlap and association proofs.
#[derive(Clone, Copy, Default)]
pub(super) struct Controls {
    pub serial: bool,
    pub wrong_association: Option<u64>,
    pub draw_error: Option<u64>,
    pub quit: Option<u64>,
    #[cfg(not(target_os = "vita"))]
    order: Option<Order>,
    #[cfg(not(target_os = "vita"))]
    delay: u64,
}

#[cfg(not(target_os = "vita"))]
#[derive(Clone, Copy)]
enum Order {
    Rendezvous,
    OwnerFirst,
    WorkerFirst,
}

impl Controls {
    pub(super) fn read() -> anyhow::Result<Self> {
        #[cfg(not(target_os = "vita"))]
        {
            let order = match std::env::var("LOOKING_GLASS_SIM_LAG_ORDER").as_deref() {
                Err(std::env::VarError::NotPresent) => None,
                Ok("rendezvous") => Some(Order::Rendezvous),
                Ok("owner-first") => Some(Order::OwnerFirst),
                Ok("worker-first") => Some(Order::WorkerFirst),
                _ => anyhow::bail!("invalid lag-one execution order"),
            };
            fn frame(name: &str) -> anyhow::Result<Option<u64>> {
                Ok(std::env::var(name).ok().map(|s| s.parse()).transpose()?)
            }
            let delay = frame("LOOKING_GLASS_SIM_LAG_DELAY_US")?.unwrap_or(0);
            anyhow::ensure!(
                [0, 2000, 5000, 12000].contains(&delay),
                "invalid lag-one delay"
            );
            Ok(Self {
                serial: std::env::var_os("LOOKING_GLASS_SIM_LAG_SERIAL").is_some(),
                wrong_association: frame("LOOKING_GLASS_SIM_LAG_WRONG_ASSOCIATION")?,
                draw_error: frame("LOOKING_GLASS_SIM_LAG_DRAW_ERROR")?,
                quit: frame("LOOKING_GLASS_SIM_LAG_QUIT")?,
                order,
                delay,
            })
        }
        #[cfg(target_os = "vita")]
        Ok(Self::default())
    }

    pub(super) fn pair(self, pending: bool) -> (Option<Peer>, Option<Peer>) {
        #[cfg(not(target_os = "vita"))]
        if let Some(order) = self.order.filter(|_| pending) {
            use std::sync::mpsc::sync_channel;
            let (owner_tx, owner_rx) = sync_channel(1);
            let (worker_tx, worker_rx) = sync_channel(1);
            let peer = |tx, rx, first| Peer {
                tx,
                rx,
                before: matches!(order, Order::Rendezvous),
                after: first,
                wait: !first,
                delay: if first || matches!(order, Order::Rendezvous) {
                    0
                } else {
                    self.delay
                },
            };
            let rendezvous = matches!(order, Order::Rendezvous);
            return (
                Some(peer(
                    owner_tx,
                    worker_rx,
                    !rendezvous && matches!(order, Order::OwnerFirst),
                )),
                Some(peer(
                    worker_tx,
                    owner_rx,
                    !rendezvous && matches!(order, Order::WorkerFirst),
                )),
            );
        }
        let _ = pending;
        (None, None)
    }
}

pub(super) struct Peer {
    #[cfg(not(target_os = "vita"))]
    tx: std::sync::mpsc::SyncSender<()>,
    #[cfg(not(target_os = "vita"))]
    rx: std::sync::mpsc::Receiver<()>,
    #[cfg(not(target_os = "vita"))]
    before: bool,
    #[cfg(not(target_os = "vita"))]
    after: bool,
    #[cfg(not(target_os = "vita"))]
    wait: bool,
    #[cfg(not(target_os = "vita"))]
    delay: u64,
}

impl Peer {
    pub(super) fn enter(&self) {
        #[cfg(not(target_os = "vita"))]
        {
            if self.before {
                self.tx.send(()).expect("missing overlap peer");
            }
            if self.wait {
                self.rx.recv().expect("missing overlap peer");
            }
            if self.delay != 0 {
                std::thread::sleep(std::time::Duration::from_micros(self.delay));
            }
        }
    }

    pub(super) fn leave(self) {
        #[cfg(not(target_os = "vita"))]
        if self.after {
            self.tx.send(()).expect("missing overlap peer");
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SaveAction {
    MenuSave,
    Resume,
    NewGame,
    NaturalExit,
    Retry,
}

#[derive(Default)]
pub(super) struct SaveActions {
    #[cfg(not(target_os = "vita"))]
    entries: Vec<(u64, SaveAction)>,
}

impl SaveActions {
    pub(super) fn read() -> anyhow::Result<Self> {
        #[cfg(not(target_os = "vita"))]
        {
            let Ok(raw) = std::env::var("LOOKING_GLASS_SIM_LAG_SAVE_ACTIONS") else {
                return Ok(Self::default());
            };
            Self::parse(&raw)
        }
        #[cfg(target_os = "vita")]
        Ok(Self::default())
    }

    #[cfg(not(target_os = "vita"))]
    fn parse(raw: &str) -> anyhow::Result<Self> {
        let mut entries = Vec::new();
        for item in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let Some((frame, kind)) = item.split_once(':') else {
                anyhow::bail!("invalid lag save action {item:?}");
            };
            let frame: u64 = frame.parse()?;
            let action = match kind {
                "menu-save" => SaveAction::MenuSave,
                "resume" => SaveAction::Resume,
                "new-game" => SaveAction::NewGame,
                "natural-exit" => SaveAction::NaturalExit,
                "retry" => SaveAction::Retry,
                _ => anyhow::bail!("unknown lag save action {kind:?}"),
            };
            entries.push((frame, action));
        }
        Ok(Self { entries })
    }

    pub(super) fn take(&mut self, frame: u64) -> Option<SaveAction> {
        #[cfg(not(target_os = "vita"))]
        {
            let index = self.entries.iter().position(|&(scheduled, _)| scheduled == frame)?;
            Some(self.entries.remove(index).1)
        }
        #[cfg(target_os = "vita")]
        {
            let _ = frame;
            None
        }
    }
}

#[cfg(all(test, not(target_os = "vita")))]
mod tests {
    use super::*;

    #[test]
    fn parse_save_actions_schedule() {
        let mut actions = SaveActions::parse("30:menu-save, 31:resume, 45:retry, 50:natural-exit").unwrap();
        assert_eq!(actions.take(29), None);
        assert_eq!(actions.take(30), Some(SaveAction::MenuSave));
        assert_eq!(actions.take(30), None);
        assert_eq!(actions.take(31), Some(SaveAction::Resume));
        assert_eq!(actions.take(45), Some(SaveAction::Retry));
        assert_eq!(actions.take(50), Some(SaveAction::NaturalExit));
        assert!(SaveActions::parse("bad").is_err());
        assert!(SaveActions::parse("30:unknown").is_err());
    }
}
