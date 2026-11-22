//! Runtime actor identities survive vector moves and never enter saves.
use std::{
    cell::Cell,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(1);
#[cfg(any(debug_assertions, not(target_os = "vita")))]
static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ActorId(u64);
thread_local! {
    #[cfg(not(target_os = "vita"))]
    static OWNER_STEP_IN_FLIGHT: Cell<bool> = const { Cell::new(false) };
    static SIM_WORKER: Cell<bool> = const { Cell::new(false) };
    static CURRENT: Cell<Option<ActorId>> = const { Cell::new(None) };
}
#[cfg(not(target_os = "vita"))]
pub(crate) struct OwnerStep(std::marker::PhantomData<std::rc::Rc<()>>);

#[cfg(not(target_os = "vita"))]
impl OwnerStep {
    pub(crate) fn begin() -> Self {
        OWNER_STEP_IN_FLIGHT.with(|flag| assert!(!flag.replace(true), "duplicate owner step guard"));
        Self(std::marker::PhantomData)
    }
}

#[cfg(not(target_os = "vita"))]
impl Drop for OwnerStep {
    fn drop(&mut self) {
        OWNER_STEP_IN_FLIGHT.with(|flag| flag.set(false));
    }
}

fn guard_actor_allocation() {
    #[cfg(not(target_os = "vita"))]
    OWNER_STEP_IN_FLIGHT.with(|flag| {
        assert!(!flag.get(), "owner allocated a skin identity while a sim step was in flight");
    });
}

pub(crate) fn enter_sim_worker() {
    SIM_WORKER.set(true);
}

// Render instances and publication generations are allocated on the owner.
// Console release builds pay a plain thread-local counter: skin, audio,
// asset and save threads exist, but safety is that only the owner allocates,
// through its own thread-local store or an exclusive art registry; debug and
// host builds keep the worker assertion.
pub(crate) fn next_art_instance() -> u64 {
    #[cfg(any(debug_assertions, not(target_os = "vita")))]
    assert!(!SIM_WORKER.get(), "sim worker allocated an art instance");
    #[cfg(any(debug_assertions, not(target_os = "vita")))]
    {
        NEXT_INSTANCE.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .expect("skin instance overflow")
    }
    #[cfg(all(not(debug_assertions), target_os = "vita"))]
    {
        thread_local! {
            static NEXT: Cell<u64> = const { Cell::new(1) };
        }
        NEXT.with(plain_counter_next)
    }
}
// Console fast-path counter step, built on every target that runs or tests
// it. Yields the previous value like `AtomicU64::fetch_update`; through
// `ok_or(id)` an overflow feeds the previous id to `Result::expect`, so the
// panic payload matches the atomic path byte for byte.
#[cfg(any(test, all(not(debug_assertions), target_os = "vita")))]
fn plain_counter_next(next: &Cell<u64>) -> u64 {
    let id = next.get();
    next.set(id.checked_add(1).ok_or(id).expect("skin instance overflow"));
    id
}
impl Default for ActorId {
    fn default() -> Self {
        guard_actor_allocation();
        Self(NEXT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .expect("skin actor identity overflow"))
    }
}
impl ActorId {
    pub(crate) fn census_id(self) -> u64 { self.0 }
    pub(crate) fn scope(self) -> Scope { Scope(CURRENT.with(|c| c.replace(Some(self)))) }
    pub(crate) fn scope_unless_set(self) -> Scope { current().unwrap_or(self).scope() }
}
pub(crate) fn current() -> Option<ActorId> { CURRENT.with(Cell::get) }
pub(crate) struct Scope(Option<ActorId>);
impl Drop for Scope { fn drop(&mut self) { CURRENT.with(|c| c.set(self.0)); } }
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scopes_preserve_actor_identity_across_shared_models_and_restore_after_draw() {
        let actor=ActorId::default(); let model=ActorId::default();
        assert_ne!(actor,model); assert_eq!(current(),None);
        { let _actor=actor.scope(); { let _model=model.scope_unless_set(); assert_eq!(current(),Some(actor)); }
          assert_eq!(current(),Some(actor)); }
        assert_eq!(current(),None);
        { let _model=model.scope_unless_set(); assert_eq!(current(),Some(model)); }
        assert_eq!(current(),None);
    }
    #[test]
    fn console_counter_step_matches_the_atomic_counter() {
        let console = Cell::new(1u64);
        let atomic = AtomicU64::new(1u64);
        for expected in 1u64..=8 {
            assert_eq!(plain_counter_next(&console), expected);
            assert_eq!(
                atomic
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
                    .expect("skin instance overflow"),
                expected,
            );
        }
        let panic_message = |payload: Box<dyn std::any::Any + Send>| match payload.downcast::<String>() {
            Ok(text) => *text,
            Err(payload) => match payload.downcast::<&'static str>() {
                Ok(text) => (*text).to_string(),
                Err(_) => panic!("panic carried no message"),
            },
        };
        let console_panic = panic_message(
            std::panic::catch_unwind(|| plain_counter_next(&Cell::new(u64::MAX))).unwrap_err(),
        );
        let atomic_panic = panic_message(std::panic::catch_unwind(|| {
            AtomicU64::new(u64::MAX)
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
                .expect("skin instance overflow")
        })
        .unwrap_err());
        assert_eq!(console_panic, atomic_panic);
        assert_eq!(console_panic, "skin instance overflow: 18446744073709551615");
    }
}
