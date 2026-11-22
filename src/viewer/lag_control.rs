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

/// Whether mute keeps a step from retaining its frame. Mute only scales the owner's sink
/// volumes. The owner applies a step's audio requests before it decides to retain that step's
/// frame, a retained presentation touches no audio, and both mute writes (M and the Start menu's
/// Audio page) flush the retained frame first, so a muted lag step hears and shows exactly what the
/// serial step would. With `sim_overlap_normal` mute therefore fences nothing; without the key it
/// fences as before, so the key-off build and the legacy diagnostics are unchanged.
pub(super) fn mute_fences_lag(sim_overlap_normal: bool, muted: bool) -> bool {
    muted && !sim_overlap_normal
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SaveAction {
    MenuSave,
    Resume,
    NewGame,
    NaturalExit,
    Retry,
    /// Kill the player and leave the death screen to the live Retry press.
    Die,
}

/// Host proof schedule of owner actions by frame (`LOOKING_GLASS_SIM_LAG_SAVE_ACTIONS`).
/// Only probe builds read it; release and Vita builds schedule nothing.
#[derive(Default)]
pub(super) struct SaveActions {
    #[cfg(any(test, all(feature = "sim_maps_probe", not(target_os = "vita"))))]
    entries: Vec<(u64, SaveAction, Option<String>)>,
}

impl SaveActions {
    pub(super) fn read() -> anyhow::Result<Self> {
        #[cfg(all(feature = "sim_maps_probe", not(target_os = "vita")))]
        {
            let Ok(raw) = std::env::var("LOOKING_GLASS_SIM_LAG_SAVE_ACTIONS") else {
                return Ok(Self::default());
            };
            Self::parse(&raw)
        }
        #[cfg(not(all(feature = "sim_maps_probe", not(target_os = "vita"))))]
        Ok(Self::default())
    }

    #[cfg(any(test, all(feature = "sim_maps_probe", not(target_os = "vita"))))]
    fn parse(raw: &str) -> anyhow::Result<Self> {
        let mut entries = Vec::new();
        for item in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let Some((frame, kind)) = item.split_once(':') else {
                anyhow::bail!("invalid lag save action {item:?}");
            };
            let frame: u64 = frame.parse()?;
            // `natural-exit=<map>` names the exit's destination; plain `natural-exit` keeps
            // LOOKING_GLASS_TRANSITION_DESTINATION.
            let (kind, destination) = match kind.split_once('=') {
                Some(("natural-exit", map)) if !map.is_empty() => ("natural-exit", Some(map.to_owned())),
                Some(_) => anyhow::bail!("invalid lag save action {item:?}"),
                None => (kind, None),
            };
            let action = match kind {
                "menu-save" => SaveAction::MenuSave,
                "resume" => SaveAction::Resume,
                "new-game" => SaveAction::NewGame,
                "natural-exit" => SaveAction::NaturalExit,
                "retry" => SaveAction::Retry,
                "die" => SaveAction::Die,
                _ => anyhow::bail!("unknown lag save action {kind:?}"),
            };
            entries.push((frame, action, destination));
        }
        Ok(Self { entries })
    }

    /// The action scheduled for this frame and, for `natural-exit=<map>`, its destination.
    /// The destination travels with the action; nothing writes the process environment.
    pub(super) fn take(&mut self, frame: u64) -> Option<(SaveAction, Option<String>)> {
        #[cfg(any(test, all(feature = "sim_maps_probe", not(target_os = "vita"))))]
        {
            let index = self.entries.iter().position(|(scheduled, _, _)| *scheduled == frame)?;
            let (_, action, destination) = self.entries.remove(index);
            Some((action, destination))
        }
        #[cfg(not(any(test, all(feature = "sim_maps_probe", not(target_os = "vita")))))]
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
    fn mute_fences_lag_only_without_the_normal_key() {
        assert!(!mute_fences_lag(false, false));
        assert!(mute_fences_lag(false, true), "key off: mute keeps fencing lag, as before");
        assert!(!mute_fences_lag(true, false));
        assert!(!mute_fences_lag(true, true), "sim_overlap_normal: a muted player keeps lag one");
    }

    #[test]
    fn parse_save_actions_schedule() {
        let mut actions = SaveActions::parse("30:menu-save, 31:resume, 45:retry, 50:natural-exit").unwrap();
        assert_eq!(actions.take(29), None);
        assert_eq!(actions.take(30), Some((SaveAction::MenuSave, None)));
        assert_eq!(actions.take(30), None);
        assert_eq!(actions.take(31), Some((SaveAction::Resume, None)));
        assert_eq!(actions.take(45), Some((SaveAction::Retry, None)));
        assert_eq!(actions.take(50), Some((SaveAction::NaturalExit, None)));
        assert!(SaveActions::parse("bad").is_err());
        let mut exits = SaveActions::parse("5:natural-exit=potears3").unwrap();
        assert_eq!(exits.take(5), Some((SaveAction::NaturalExit, Some("potears3".to_owned()))));
        assert!(std::env::var_os("LOOKING_GLASS_TRANSITION_DESTINATION").is_none(),
            "the destination must not reach the process environment");
        assert_eq!(SaveActions::parse("7:die").unwrap().take(7), Some((SaveAction::Die, None)));
        assert!(SaveActions::parse("5:natural-exit=").is_err());
        assert!(SaveActions::parse("5:retry=potears3").is_err());
        assert!(SaveActions::parse("30:unknown").is_err());
    }
}
